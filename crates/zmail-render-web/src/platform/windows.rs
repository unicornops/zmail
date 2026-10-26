//! The Windows web renderer: a locked-down WebView2 per message, hosted as
//! a DirectComposition visual.
//!
//! With the macOS module, this is the only code in Zmail that uses
//! `unsafe`: it talks to WebView2 through `webview2-com` and to Win32
//! through the `windows` crate. Every `unsafe` block says why it is sound.
//! The rest of the crate sees a small safe API: a [`VisualTarget`] (the
//! window and the DirectComposition visual a page draws into) and a
//! [`PlatformWebView`] per message.
//!
//! Each web view is a WebView2 *composition controller*: it draws into a
//! visual of GPUI's own visual tree (the native surface's), so Zmail's
//! overlays draw above it, and it has no window of its own, so it gets
//! input only from Zmail (`platform::windows_host`). Each is hardened as
//! ADR 0006 §3 requires:
//!
//! - Page JavaScript is off (`IsScriptEnabled = false`). Zmail's own
//!   scripts (measuring, Copy, Select All) run through `ExecuteScript`,
//!   which that setting does not stop; with page scripts off there is no
//!   page script to see them.
//! - The page comes from Zmail's `zmail-mail:` scheme (a registered custom
//!   scheme, answered in `WebResourceRequested`) with a CSP response header
//!   and a `<meta>` copy; `cid:` parts come from the same handler, and so
//!   do remote images the user allowed, which Zmail fetches itself
//!   (`crate::remote`). A `*` filter sends every other request there too,
//!   and each is answered with an empty 403, so the engine never connects
//!   anywhere.
//! - Navigation is cancelled except for the one page load Zmail starts;
//!   link clicks are reported to Zmail, which asks before opening anything.
//!   Frames never navigate, new windows are dropped, external schemes,
//!   downloads, permissions and basic authentication are denied.
//! - No dev tools, context menus, web messages, host objects, status bar,
//!   zoom control, accelerator keys, pinch zoom, swipe navigation,
//!   autofill, password saving or SmartScreen lookups.
//! - Each view has its own InPrivate profile with tracking prevention set
//!   to strict, in an environment whose user data folder is Zmail's own,
//!   under the temporary folder, and is deleted when the browser process
//!   exits (and on a later start if the process died first).
//!
//! The runtime is the system's evergreen WebView2. When it is missing or
//! the environment cannot be created, [`unavailable_reason`] says why and
//! the reader keeps its text view.

#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::MouseButton;
use webview2_com::{
    BasicAuthenticationRequestedEventHandler, BrowserProcessExitedEventHandler,
    ContextMenuRequestedEventHandler, CoreWebView2CustomSchemeRegistration,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, CursorChangedEventHandler,
    DownloadStartingEventHandler, ExecuteScriptCompletedHandler, FocusChangedEventHandler,
    LaunchingExternalUriSchemeEventHandler, Microsoft::Web::WebView2::Win32::*,
    MoveFocusRequestedEventHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler,
    PermissionRequestedEventHandler, ProcessFailedEventHandler, WebResourceRequestedEventHandler,
    take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, E_ACCESSDENIED, E_POINTER, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM,
            LRESULT, POINT, RECT, STILL_ACTIVE, WPARAM,
        },
        Graphics::DirectComposition::{
            DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget,
            IDCompositionVisual,
        },
        System::{
            Com::IStream,
            DataExchange::{
                CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
            },
            LibraryLoader::GetModuleHandleW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{CF_UNICODETEXT, OleInitialize},
            Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
        },
        UI::{
            Input::KeyboardAndMouse::SetFocus,
            Shell::SHCreateMemStream,
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, KillTimer, MSG,
                PM_REMOVE, PeekMessageW, RegisterClassW, SetTimer, TranslateMessage, WNDCLASSW,
                WS_EX_NOACTIVATE, WS_POPUP, WS_VISIBLE,
            },
        },
    },
    core::{BOOL, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, w},
};

use crate::document::{
    CID_SCHEME, InlineResource, LINK_SCHEME, MAIL_SCHEME, SchemeRequest, WebDocument,
    content_security_policy, page_html, page_token, page_url, parse_link_id, parse_scheme_request,
};
use crate::remote::{FetchResult, RemoteError, RemoteFetcher};
use crate::script::{
    MAX_HEIGHT, MEASURE_SCRIPT, Measurement, SELECT_ALL_SCRIPT, SELECTION_SCRIPT,
    decode_json_string,
};
use crate::{WebEvent, debug_log};

/// How often a waiting remote image fetch is checked.
const FETCH_POLL: Duration = Duration::from_millis(30);

/// How long to wait before measuring again, while the page has not yet
/// laid out at its new width or zoom.
const RELAYOUT_WAIT: Duration = Duration::from_millis(50);

/// How many times a page is measured again before its measurement is taken
/// as it is.
const MAX_RELAYOUT_WAITS: u32 = 20;

/// The folder, under the temporary folder, that holds each Zmail process's
/// WebView2 user data folder.
const USER_DATA_DIR: &str = "zmail-webview2";

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

thread_local! {
    /// What each pending one-shot thread timer runs, by timer id.
    static TIMERS: RefCell<HashMap<usize, Box<dyn FnOnce()>>> = RefCell::new(HashMap::new());
}

/// Runs `then` on this (the UI) thread after `delay`, from its message
/// loop.
fn after(delay: Duration, then: impl FnOnce() + 'static) {
    let millis = u32::try_from(delay.as_millis()).unwrap_or(u32::MAX).max(1);
    // SAFETY: a thread timer (no window) with a callback that has the
    // TIMERPROC signature. The message loop of this thread (GPUI's, or the
    // canary's pump) dispatches its WM_TIMER, which calls `timer_fired` on
    // this thread.
    let id = unsafe { SetTimer(None, 0, millis, Some(timer_fired)) };
    if id == 0 {
        eprintln!("zmail: web renderer: could not start a timer");
        return;
    }
    TIMERS.with(|timers| timers.borrow_mut().insert(id, Box::new(then)));
}

unsafe extern "system" fn timer_fired(_window: HWND, _message: u32, id: usize, _time: u32) {
    // SAFETY: stops the thread timer this call is for, so it fires once.
    let _ = unsafe { KillTimer(None, id) };
    let then = TIMERS.with(|timers| timers.borrow_mut().remove(&id));
    if let Some(then) = then {
        then();
    }
}

// ---------------------------------------------------------------------------
// The environment
// ---------------------------------------------------------------------------

/// What waits for the environment: the environment, or why there is none.
type EnvironmentWaiter = Box<dyn FnOnce(Result<ICoreWebView2Environment10, String>)>;

/// The process's WebView2 environment.
enum Environment {
    Idle,
    Creating(Vec<EnvironmentWaiter>),
    Ready {
        environment: ICoreWebView2Environment10,
        folder: PathBuf,
    },
    Failed(String),
}

thread_local! {
    static ENVIRONMENT: RefCell<Environment> = const { RefCell::new(Environment::Idle) };
    /// Whether a WebView2 runtime is installed, checked once.
    static RUNTIME: RefCell<Option<Option<String>>> = const { RefCell::new(None) };
    /// Environments created so far, for a fresh user data folder each.
    static FOLDERS: Cell<u32> = const { Cell::new(0) };
}

/// Why the web renderer cannot run on this machine, if it cannot: no
/// WebView2 runtime, or the environment could not be created.
pub(crate) fn unavailable_reason() -> Option<String> {
    if let Some(reason) = ENVIRONMENT.with(|environment| match &*environment.borrow() {
        Environment::Failed(reason) => Some(reason.clone()),
        _ => None,
    }) {
        return Some(reason);
    }
    RUNTIME.with(|runtime| {
        runtime
            .borrow_mut()
            .get_or_insert_with(|| {
                let mut version = PWSTR::null();
                // SAFETY: a null folder asks for the installed evergreen
                // runtime; WebView2 writes a CoTaskMemAlloc string (or null),
                // which `take_pwstr` frees.
                let result = unsafe {
                    GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut version)
                };
                match result {
                    Err(error) => Some(format!("the WebView2 runtime is not available ({error})")),
                    Ok(()) => {
                        let version = take_pwstr(version);
                        if version.is_empty() {
                            Some("the WebView2 runtime is not installed".to_owned())
                        } else {
                            debug_log(format_args!("WebView2 runtime {version}"));
                            None
                        }
                    }
                }
            })
            .clone()
    })
}

/// Whether a WebView2 browser process may still be running for this
/// process: an environment is being created or has not seen its browser
/// process exit. For the canary test, which waits for it before exiting.
pub fn browser_running() -> bool {
    ENVIRONMENT.with(|environment| {
        matches!(
            &*environment.borrow(),
            Environment::Creating(_) | Environment::Ready { .. }
        )
    })
}

/// Calls `then` with the environment, creating it first if needed. `then`
/// may run later, from the message loop.
fn with_environment(then: impl FnOnce(Result<ICoreWebView2Environment10, String>) + 'static) {
    enum Next {
        Now(Result<ICoreWebView2Environment10, String>),
        Wait,
        Create,
    }
    let mut then = Some(Box::new(then) as EnvironmentWaiter);
    let next = ENVIRONMENT.with(|environment| {
        let mut environment = environment.borrow_mut();
        match &mut *environment {
            Environment::Ready { environment, .. } => Next::Now(Ok(environment.clone())),
            Environment::Failed(reason) => Next::Now(Err(reason.clone())),
            Environment::Creating(waiting) => {
                waiting.extend(then.take());
                Next::Wait
            }
            Environment::Idle => {
                *environment = Environment::Creating(then.take().into_iter().collect());
                Next::Create
            }
        }
    });
    match next {
        Next::Now(result) => {
            if let Some(then) = then {
                then(result);
            }
        }
        Next::Wait => {}
        Next::Create => {
            if let Err(error) = create_environment() {
                environment_finished(Err(format!("{error:#}")), None);
            }
        }
    }
}

/// Starts creating the environment; `environment_finished` hears the end.
fn create_environment() -> Result<()> {
    let base = std::env::temp_dir().join(USER_DATA_DIR);
    remove_stale_folders(&base);
    let count = FOLDERS.with(|folders| folders.replace(folders.get() + 1));
    let folder = base.join(format!("{}-{count}", std::process::id()));
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    debug_log(format_args!(
        "creating the WebView2 environment in {}",
        folder.display()
    ));

    let options = CoreWebView2EnvironmentOptions::default();
    let schemes: Vec<Option<ICoreWebView2CustomSchemeRegistration>> = [
        (MAIL_SCHEME, true),
        (CID_SCHEME, false),
        (LINK_SCHEME, false),
    ]
    .into_iter()
    .map(|(scheme, authority)| {
        let registration = CoreWebView2CustomSchemeRegistration::new(scheme.to_owned());
        // SAFETY: setters on a registration object we own, before WebView2
        // sees it.
        unsafe {
            registration.set_treat_as_secure(true);
            registration.set_has_authority_component(authority);
            registration.set_allowed_origins(vec![format!("{MAIL_SCHEME}://message")]);
        }
        Some(registration.into())
    })
    .collect();
    // SAFETY: setters on an options object we own, before WebView2 sees it.
    unsafe {
        options.set_scheme_registrations(schemes);
        options.set_exclusive_user_data_folder_access(true);
        // Crash dumps stay in the user data folder, which Zmail deletes,
        // instead of going to Microsoft.
        options.set_is_custom_crash_reporting_enabled(true);
        options.set_enable_tracking_prevention(true);
        options.set_are_browser_extensions_enabled(false);
        options.set_allow_single_sign_on_using_os_primary_account(false);
    }
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let created_folder = folder.clone();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(
        move |result, environment| {
            let environment = result
                .and_then(|()| environment.ok_or_else(|| windows::core::Error::from(E_POINTER)));
            match environment {
                Ok(environment) => environment_created(&environment, created_folder),
                Err(error) => environment_finished(
                    Err(format!(
                        "the WebView2 environment was not created ({error})"
                    )),
                    Some(created_folder),
                ),
            }
            Ok(())
        },
    ));
    // SAFETY: a null browser folder (the evergreen runtime), a folder path
    // that lives through the call, and COM objects we hold; WebView2 calls
    // the handler once, on this thread.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &HSTRING::from(folder.as_path()),
            &options,
            &handler,
        )
    }
    .context("creating the WebView2 environment")
}

fn environment_created(environment: &ICoreWebView2Environment, folder: PathBuf) {
    let Ok(environment) = environment.cast::<ICoreWebView2Environment10>() else {
        environment_finished(
            Err("the WebView2 runtime is too old (no ICoreWebView2Environment10)".to_owned()),
            Some(folder),
        );
        return;
    };
    // When the browser process exits (every web view closed, or a crash)
    // the folder is deleted and the next web view starts a new
    // environment.
    if let Ok(environment5) = environment.cast::<ICoreWebView2Environment5>() {
        let exited_folder = folder.clone();
        let handler = BrowserProcessExitedEventHandler::create(Box::new(move |_, _| {
            browser_exited(&exited_folder);
            Ok(())
        }));
        let mut token = 0;
        // SAFETY: registers a handler we created on a live environment.
        if let Err(error) = unsafe { environment5.add_BrowserProcessExited(&handler, &mut token) } {
            eprintln!("zmail: web renderer: no browser process exit handler: {error}");
        }
    }
    debug_log(format_args!("WebView2 environment created"));
    environment_finished(Ok(environment), Some(folder));
}

/// Settles the environment and answers everyone waiting for it.
fn environment_finished(
    result: Result<ICoreWebView2Environment10, String>,
    folder: Option<PathBuf>,
) {
    let waiting = ENVIRONMENT.with(|environment| {
        let mut environment = environment.borrow_mut();
        let waiting = match &mut *environment {
            Environment::Creating(waiting) => std::mem::take(waiting),
            _ => Vec::new(),
        };
        *environment = match (&result, folder) {
            (Ok(created), Some(folder)) => Environment::Ready {
                environment: created.clone(),
                folder,
            },
            (Ok(_), None) => Environment::Idle,
            (Err(reason), folder) => {
                eprintln!("zmail: web renderer: {reason}");
                if let Some(folder) = folder {
                    remove_folder(&folder);
                }
                Environment::Failed(reason.clone())
            }
        };
        waiting
    });
    for then in waiting {
        then(result.clone());
    }
}

fn browser_exited(folder: &Path) {
    debug_log(format_args!("the WebView2 browser process exited"));
    ENVIRONMENT.with(|environment| {
        let mut environment = environment.borrow_mut();
        if matches!(&*environment, Environment::Ready { folder: current, .. } if current == folder)
        {
            *environment = Environment::Idle;
        }
    });
    remove_folder(folder);
}

fn remove_folder(folder: &Path) {
    match std::fs::remove_dir_all(folder) {
        Ok(()) => debug_log(format_args!("removed {}", folder.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => eprintln!(
            "zmail: web renderer: could not remove {}: {error}",
            folder.display()
        ),
    }
}

/// Removes the user data folders of Zmail processes that are gone (a
/// crash, or an exit before the browser process exited).
fn remove_stale_folders(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    let own = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name
            .to_str()
            .and_then(|name| name.split('-').next())
            .and_then(|pid| pid.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != own && !process_running(pid) {
            remove_folder(&entry.path());
        }
    }
}

fn process_running(pid: u32) -> bool {
    // SAFETY: opens a query-only handle to the process, which is closed
    // before returning; `code` outlives the call that writes it.
    unsafe {
        match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(handle) => {
                let mut code = 0u32;
                let read = GetExitCodeProcess(handle, &mut code).is_ok();
                let _ = CloseHandle(handle);
                read && code == STILL_ACTIVE.0 as u32
            }
            // Another user's process: it exists.
            Err(error) => error.code() == E_ACCESSDENIED,
        }
    }
}

// ---------------------------------------------------------------------------
// The visual target
// ---------------------------------------------------------------------------

/// Where a web view draws: a DirectComposition visual (a native composition
/// surface's), in a window that hosts its input, focus and accessibility.
#[derive(Clone)]
pub struct VisualTarget {
    window: HWND,
    visual: IUnknown,
}

impl VisualTarget {
    /// `window` is the GPUI window's `HWND` (from its raw window handle),
    /// `visual` the native surface's platform handle.
    pub(crate) fn new(window: isize, visual: IUnknown) -> Self {
        Self {
            window: HWND(window as *mut c_void),
            visual,
        }
    }
}

/// Takes the children out of a native surface's visual, before GPUI adds
/// them back.
///
/// GPUI's `set_surface_order` (Zed PR #62379,
/// `gpui_windows/src/directx_renderer.rs`) empties the root visual and
/// GPUI's own visuals, then adds every surface to its parent's visual, but
/// a native parent's visual is never emptied, and DirectComposition refuses
/// to add a visual that already has a parent. The host calls this right
/// before anything that makes GPUI rebuild its tree.
pub(crate) fn detach_children(visual: &IUnknown) {
    match visual.cast::<IDCompositionVisual>() {
        // SAFETY: `visual` is a live visual of GPUI's device (the viewport
        // surface's); removing its children is a pending change that GPUI's
        // rebuild, which adds them back, commits.
        Ok(visual) => {
            if let Err(error) = unsafe { visual.RemoveAllVisuals() } {
                eprintln!("zmail: web renderer: could not detach the message surfaces: {error}");
            }
        }
        Err(error) => eprintln!("zmail: web renderer: not a DirectComposition visual: {error}"),
    }
}

// ---------------------------------------------------------------------------
// Mouse input
// ---------------------------------------------------------------------------

/// A mouse event forwarded to a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    /// The pointer moved over the page.
    Move,
    /// The pointer left the page.
    Leave,
    /// A button went down.
    Down(Button),
    /// A button went down a second time in quick succession.
    DoubleClick(Button),
    /// A button came up.
    Up(Button),
    /// A sideways wheel turn.
    HorizontalWheel,
}

/// A mouse button a page hears about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// The left button.
    Left,
    /// The right button.
    Right,
    /// The middle button.
    Middle,
}

impl MouseKind {
    /// A press of GPUI's `button` (back and forward buttons are not passed
    /// on).
    pub(crate) fn down(button: MouseButton, click_count: usize) -> Option<Self> {
        let button = Button::from_gpui(button)?;
        Some(if click_count == 2 {
            Self::DoubleClick(button)
        } else {
            Self::Down(button)
        })
    }

    /// A release of GPUI's `button`.
    pub(crate) fn up(button: MouseButton) -> Option<Self> {
        Button::from_gpui(button).map(Self::Up)
    }

    fn to_webview2(self) -> COREWEBVIEW2_MOUSE_EVENT_KIND {
        use Button::{Left, Middle, Right};
        match self {
            Self::Move => COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE,
            Self::Leave => COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
            Self::Down(Left) => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN,
            Self::Down(Right) => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN,
            Self::Down(Middle) => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN,
            Self::DoubleClick(Left) => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK,
            Self::DoubleClick(Right) => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOUBLE_CLICK,
            Self::DoubleClick(Middle) => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOUBLE_CLICK,
            Self::Up(Left) => COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP,
            Self::Up(Right) => COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP,
            Self::Up(Middle) => COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP,
            Self::HorizontalWheel => COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL,
        }
    }
}

impl Button {
    fn from_gpui(button: MouseButton) -> Option<Self> {
        match button {
            MouseButton::Left => Some(Self::Left),
            MouseButton::Right => Some(Self::Right),
            MouseButton::Middle => Some(Self::Middle),
            MouseButton::Navigate(_) => None,
        }
    }
}

/// The buttons and keys held during a mouse event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseKeys {
    /// The left button is down.
    pub left: bool,
    /// The right button is down.
    pub right: bool,
    /// The middle button is down.
    pub middle: bool,
    /// Shift is down.
    pub shift: bool,
    /// Control is down.
    pub control: bool,
}

impl MouseKeys {
    fn to_webview2(self) -> COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS {
        let mut keys = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE.0;
        for (held, key) in [
            (self.left, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON),
            (
                self.right,
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON,
            ),
            (
                self.middle,
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON,
            ),
            (self.shift, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT),
            (self.control, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL),
        ] {
            if held {
                keys |= key.0;
            }
        }
        COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(keys)
    }
}

// ---------------------------------------------------------------------------
// The web view
// ---------------------------------------------------------------------------

/// The page a web view serves.
struct ServedPage {
    token: String,
    url: String,
    html: Vec<u8>,
    policy: String,
    resources: HashMap<String, InlineResource>,
    /// Remote images Zmail may fetch, by the `n` of
    /// `zmail-mail://remote/<n>`.
    remote: Vec<Option<String>>,
}

/// A remote image being fetched for a deferred request.
struct PendingFetch {
    args: ICoreWebView2WebResourceRequestedEventArgs,
    deferral: ICoreWebView2Deferral,
    result: Receiver<FetchResult>,
}

/// The WebView2 objects of a created view.
struct Web {
    environment: ICoreWebView2Environment10,
    composition: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
}

struct ViewState {
    on_event: Rc<dyn Fn(WebEvent)>,
    target: RefCell<VisualTarget>,
    page: ServedPage,
    dark: bool,
    web: RefCell<Option<Web>>,
    /// The view's size in pixels.
    size: Cell<(i32, i32)>,
    /// Device pixels per DIP (one CSS pixel at zoom 1).
    scale: Cell<f64>,
    hidden: Cell<bool>,
    /// The id of Zmail's own page load, once started.
    navigation: Cell<Option<u64>>,
    /// The page finished loading, so it can be measured.
    loaded: Cell<bool>,
    /// The next measurement may zoom the page out to fit its width.
    fit_pending: Cell<bool>,
    /// The width, in pixels, the page was last fitted at.
    fitted_width: Cell<i32>,
    zoom: Cell<f64>,
    /// Measurements taken again while the page catches up with a new width
    /// or zoom.
    relayout_waits: Cell<u32>,
    /// The page's cursor, a system cursor id (`IDC_*`).
    cursor: Cell<u32>,
    pending: RefCell<Vec<PendingFetch>>,
    polling: Cell<bool>,
}

/// One message's web view. Clones share the view, which closes when the
/// last one is dropped.
#[derive(Clone)]
pub struct PlatformWebView {
    state: Rc<ViewState>,
}

impl PlatformWebView {
    /// Starts showing `document` in `target`. The page loads once the
    /// environment and the view are created (asynchronously); `on_event`
    /// hears about measurements, link clicks and failures.
    pub fn new(
        target: &VisualTarget,
        document: &WebDocument,
        on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<Self> {
        if let Some(reason) = unavailable_reason() {
            bail!(reason);
        }
        let token = page_token();
        let state = Rc::new(ViewState {
            on_event,
            target: RefCell::new(target.clone()),
            page: ServedPage {
                url: page_url(&token),
                token,
                html: page_html(document).into_bytes(),
                policy: content_security_policy(true),
                resources: document.resources_by_id(),
                remote: document.remote.clone(),
            },
            dark: document.draws_dark(),
            web: RefCell::new(None),
            size: Cell::new((1, 1)),
            scale: Cell::new(1.),
            hidden: Cell::new(true),
            navigation: Cell::new(None),
            loaded: Cell::new(false),
            fit_pending: Cell::new(true),
            fitted_width: Cell::new(0),
            zoom: Cell::new(1.),
            relayout_waits: Cell::new(0),
            cursor: Cell::new(0),
            pending: RefCell::new(Vec::new()),
            polling: Cell::new(false),
        });
        let weak = Rc::downgrade(&state);
        with_environment(move |environment| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let result = environment
                .map_err(|reason| anyhow!(reason))
                .and_then(|environment| ViewState::create(&state, environment));
            if let Err(error) = result {
                eprintln!("zmail: web renderer: the web view was not created: {error:#}");
                (state.on_event)(WebEvent::LoadFailed(format!("{error:#}")));
            }
        });
        Ok(Self { state })
    }

    /// Sizes the view, in pixels.
    pub fn set_size(&self, width: i32, height: i32) {
        let size = (width.max(1), height.max(1));
        if self.state.size.replace(size) != size {
            self.state.apply_bounds();
        }
    }

    /// Sets how many pixels a CSS pixel takes (the window's scale factor).
    pub fn set_scale(&self, scale: f64) {
        if (self.state.scale.replace(scale) - scale).abs() > f64::EPSILON {
            if let Some(web) = &*self.state.web.borrow()
                && let Ok(controller) = web.controller.cast::<ICoreWebView2Controller3>()
            {
                // SAFETY: a plain property write on a live controller.
                let _ = unsafe { controller.SetRasterizationScale(scale) };
            }
            // The page's CSS width changed: fit and measure again.
            self.state.fitted_width.set(-1);
            self.state.refit();
        }
    }

    /// Hides or shows the view.
    pub fn set_hidden(&self, hidden: bool) {
        if self.state.hidden.replace(hidden) != hidden
            && let Some(web) = &*self.state.web.borrow()
        {
            // SAFETY: a plain property write on a live controller.
            let _ = unsafe { web.controller.SetIsVisible(!hidden) };
        }
    }

    /// Moves the view to a new visual: GPUI recreated its compositor.
    pub(crate) fn retarget(&self, visual: &IUnknown) {
        self.state.target.borrow_mut().visual = visual.clone();
        if let Some(web) = &*self.state.web.borrow() {
            // SAFETY: connects the live view to a live visual of GPUI's new
            // device; GPUI commits that device.
            if let Err(error) = unsafe { web.composition.SetRootVisualTarget(visual) } {
                eprintln!(
                    "zmail: web renderer: could not move a page to the new compositor: {error}"
                );
            }
        }
    }

    /// Passes a mouse event to the page; `x` and `y` are pixels from its
    /// top left corner, `wheel` a wheel delta for wheel events.
    pub fn send_mouse(&self, kind: MouseKind, keys: MouseKeys, wheel: i32, x: i32, y: i32) {
        if let Some(web) = &*self.state.web.borrow() {
            // SAFETY: a plain call on a live composition controller with a
            // point that lives through the call. A wheel delta is passed as
            // its two's complement bits, as WebView2 expects.
            let result = unsafe {
                web.composition.SendMouseInput(
                    kind.to_webview2(),
                    keys.to_webview2(),
                    wheel as u32,
                    POINT { x, y },
                )
            };
            if let Err(error) = result {
                debug_log(format_args!("mouse input {kind:?} not delivered: {error}"));
            }
        }
    }

    /// The page's cursor, as a system cursor id (`IDC_*`), or 0.
    pub fn cursor_id(&self) -> u32 {
        self.state.cursor.get()
    }

    /// Copies the page's selection to the clipboard.
    pub fn copy(&self) {
        let window = self.state.target.borrow().window;
        self.state
            .run_script(SELECTION_SCRIPT, move |result| match result {
                Ok(json) => match decode_json_string(&json) {
                    Some(text) if !text.is_empty() => {
                        if let Err(error) = set_clipboard_text(window, &text) {
                            eprintln!("zmail: web renderer: could not copy: {error:#}");
                        } else {
                            debug_log(format_args!("copied {} characters", text.chars().count()));
                        }
                    }
                    _ => debug_log(format_args!("nothing selected to copy")),
                },
                Err(error) => {
                    eprintln!("zmail: web renderer: could not read the selection: {error}")
                }
            });
    }

    /// Selects the whole page.
    pub fn select_all(&self) {
        self.state.run_script(SELECT_ALL_SCRIPT, |result| {
            if let Err(error) = result {
                eprintln!("zmail: web renderer: could not select the page: {error}");
            }
        });
    }

    /// Runs `script` and hands its string result to `then`. For the canary
    /// test (`tests/canary.rs`).
    pub fn evaluate(&self, script: &str, then: impl Fn(Result<String, String>) + 'static) {
        if self.state.web.borrow().is_none() {
            then(Err("the web view does not exist yet".to_owned()));
            return;
        }
        self.state.run_script(script, move |result| {
            then(result.and_then(|json| {
                decode_json_string(&json).ok_or_else(|| format!("not a string: {json}"))
            }));
        });
    }
}

impl ViewState {
    /// Creates the composition controller in its own InPrivate profile.
    fn create(state: &Rc<Self>, environment: ICoreWebView2Environment10) -> Result<()> {
        // SAFETY: plain calls on a live environment and the options object
        // it creates.
        let options = unsafe {
            let options = environment
                .CreateCoreWebView2ControllerOptions()
                .context("creating controller options")?;
            options.SetProfileName(&HSTRING::from(format!("zmail{}", state.page.token)))?;
            options.SetIsInPrivateModeEnabled(true)?;
            options
        };
        let weak = Rc::downgrade(state);
        let attach_environment = environment.clone();
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(
            move |result, composition| {
                let composition = result.and_then(|()| {
                    composition.ok_or_else(|| windows::core::Error::from(E_POINTER))
                });
                let Some(state) = weak.upgrade() else {
                    // The message went away first: close the view at once.
                    if let Ok(controller) = composition
                        .and_then(|composition| composition.cast::<ICoreWebView2Controller>())
                    {
                        // SAFETY: closes a live controller nobody else holds.
                        let _ = unsafe { controller.Close() };
                    }
                    return Ok(());
                };
                let result = composition
                    .map_err(|error| anyhow!("the web view was not created ({error})"))
                    .and_then(|composition| {
                        ViewState::attach(&state, attach_environment.clone(), composition)
                    });
                if let Err(error) = result {
                    eprintln!("zmail: web renderer: {error:#}");
                    (state.on_event)(WebEvent::LoadFailed(format!("{error:#}")));
                }
                Ok(())
            },
        ));
        let window = state.target.borrow().window;
        // SAFETY: `window` is the live GPUI (or test) window that hosts the
        // view; the options and handler are COM objects we hold. WebView2
        // calls the handler once, on this thread.
        unsafe {
            environment
                .CreateCoreWebView2CompositionControllerWithOptions(window, &options, &handler)
        }
        .context("creating the web view")
    }

    /// Hardens a new view, connects it to its visual and loads the page.
    fn attach(
        state: &Rc<Self>,
        environment: ICoreWebView2Environment10,
        composition: ICoreWebView2CompositionController,
    ) -> Result<()> {
        let controller: ICoreWebView2Controller = composition.cast()?;
        // SAFETY: a plain property read on the live controller.
        let webview = unsafe { controller.CoreWebView2() }?;
        harden(&webview, state.dark)?;
        // SAFETY: plain property writes on the live controllers; the visual
        // is a live visual of the window's compositor, which the host
        // commits.
        unsafe {
            if let Ok(controller3) = controller.cast::<ICoreWebView2Controller3>() {
                controller3.SetShouldDetectMonitorScaleChanges(false)?;
                controller3.SetBoundsMode(COREWEBVIEW2_BOUNDS_MODE_USE_RAW_PIXELS)?;
                controller3.SetRasterizationScale(state.scale.get())?;
            }
            controller.SetZoomFactor(1.)?;
            controller.SetIsVisible(!state.hidden.get())?;
            composition
                .SetRootVisualTarget(&state.target.borrow().visual)
                .context("connecting the web view to its visual")?;
        }
        *state.web.borrow_mut() = Some(Web {
            environment: environment.clone(),
            composition: composition.clone(),
            controller: controller.clone(),
            webview: webview.clone(),
        });
        state.apply_bounds();
        ViewState::listen(state, &environment, &composition, &controller, &webview)?;
        // SAFETY: every request goes to our handler, which answers each one
        // (see `resource_requested`); the URL is Zmail's page.
        unsafe {
            webview
                .AddWebResourceRequestedFilter(w!("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
            webview.Navigate(&HSTRING::from(state.page.url.as_str()))?;
        }
        debug_log(format_args!(
            "loading {} (dark: {})",
            state.page.url, state.dark
        ));
        Ok(())
    }

    /// Registers the event handlers. Each holds the state weakly.
    fn listen(
        state: &Rc<Self>,
        environment: &ICoreWebView2Environment10,
        composition: &ICoreWebView2CompositionController,
        controller: &ICoreWebView2Controller,
        webview: &ICoreWebView2,
    ) -> Result<()> {
        let mut token = 0;
        // SAFETY: each call registers a handler we created on a live
        // object; the handlers run on this thread, with arguments that live
        // through the call.
        unsafe {
            let weak = Rc::downgrade(state);
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                        state.navigation_starting(&args)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            webview.add_FrameNavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let weak = Rc::downgrade(state);
            webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                        state.navigation_completed(&args)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let weak = Rc::downgrade(state);
            let environment = environment.clone();
            webview.add_WebResourceRequested(
                &WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                    if let Some(args) = args {
                        match weak.upgrade() {
                            Some(state) => state.resource_requested(&environment, &args)?,
                            // The view is going away: refuse.
                            None => args.SetResponse(&blocked(&environment)?)?,
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        // Handled, with no new window: nothing opens.
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            webview.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetState(COREWEBVIEW2_PERMISSION_STATE_DENY)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let weak = Rc::downgrade(state);
            webview.add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    if let (Some(state), Some(args)) = (weak.upgrade(), args) {
                        state.process_failed(&args)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let webview4: ICoreWebView2_4 = webview
                .cast()
                .context("the WebView2 runtime is too old (no ICoreWebView2_4)")?;
            webview4.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let webview10: ICoreWebView2_10 = webview
                .cast()
                .context("the WebView2 runtime is too old (no ICoreWebView2_10)")?;
            webview10.add_BasicAuthenticationRequested(
                &BasicAuthenticationRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetCancel(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let webview11: ICoreWebView2_11 = webview
                .cast()
                .context("the WebView2 runtime is too old (no ICoreWebView2_11)")?;
            webview11.add_ContextMenuRequested(
                &ContextMenuRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            match webview.cast::<ICoreWebView2_18>() {
                Ok(webview18) => webview18.add_LaunchingExternalUriScheme(
                    &LaunchingExternalUriSchemeEventHandler::create(Box::new(|_, args| {
                        if let Some(args) = args {
                            args.SetCancel(true)?;
                        }
                        Ok(())
                    })),
                    &mut token,
                )?,
                // Older runtimes ask the user before launching another app,
                // and navigation to it is cancelled first anyway.
                Err(_) => debug_log(format_args!("no LaunchingExternalUriScheme event")),
            }

            let weak = Rc::downgrade(state);
            composition.add_CursorChanged(
                &CursorChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(state), Some(sender)) = (weak.upgrade(), sender) {
                        let mut cursor = 0u32;
                        sender.SystemCursorId(&mut cursor)?;
                        state.cursor.set(cursor);
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            // Keyboard focus stays with GPUI, so Zmail's shortcuts keep
            // working after a click in a message; Copy and Select All reach
            // the page through Zmail's commands.
            let weak = Rc::downgrade(state);
            controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    let weak = weak.clone();
                    after(Duration::ZERO, move || {
                        if let Some(state) = weak.upgrade() {
                            focus_window(state.target.borrow().window);
                        }
                    });
                    Ok(())
                })),
                &mut token,
            )?;
            controller.add_MoveFocusRequested(
                &MoveFocusRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        args.SetHandled(true)?;
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    /// Allows exactly one navigation: Zmail's page load. A click on a link
    /// is cancelled and reported.
    fn navigation_starting(
        &self,
        args: &ICoreWebView2NavigationStartingEventArgs,
    ) -> windows::core::Result<()> {
        let mut uri = PWSTR::null();
        let mut user = BOOL::default();
        let mut id = 0u64;
        // SAFETY: plain property reads on the arguments WebView2 gave us;
        // `take_pwstr` frees the string it allocated.
        let uri = unsafe {
            args.Uri(&mut uri)?;
            args.IsUserInitiated(&mut user)?;
            args.NavigationId(&mut id)?;
            take_pwstr(uri)
        };
        let ours = self.navigation.get().is_none()
            && parse_scheme_request(&uri) == SchemeRequest::Page(self.page.token.clone());
        if ours {
            self.navigation.set(Some(id));
            return Ok(());
        }
        // SAFETY: as above.
        unsafe { args.SetCancel(true)? };
        // A sanitised link is a `zmail-link:` id; only a click navigates
        // to one (there are no scripts, and no refresh survives sanitising).
        if user.as_bool() || parse_link_id(&uri).is_some() {
            debug_log(format_args!("link clicked: {uri}"));
            (self.on_event)(WebEvent::LinkActivated { url: uri });
        } else {
            debug_log(format_args!("navigation denied: {uri}"));
        }
        Ok(())
    }

    fn navigation_completed(
        self: &Rc<Self>,
        args: &ICoreWebView2NavigationCompletedEventArgs,
    ) -> windows::core::Result<()> {
        let mut id = 0u64;
        let mut success = BOOL::default();
        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS_UNKNOWN;
        // SAFETY: plain property reads on the arguments WebView2 gave us.
        unsafe {
            args.NavigationId(&mut id)?;
            args.IsSuccess(&mut success)?;
            args.WebErrorStatus(&mut status)?;
        }
        if self.navigation.get() != Some(id) {
            return Ok(());
        }
        if success.as_bool() {
            debug_log(format_args!("page loaded"));
            self.loaded.set(true);
            self.fit_pending.set(true);
            self.fitted_width.set(self.size.get().0);
            self.measure();
        } else if status != COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED {
            let message = format!("the message did not load (WebView2 error {})", status.0);
            eprintln!("zmail: web renderer: {message}");
            (self.on_event)(WebEvent::LoadFailed(message));
        }
        Ok(())
    }

    /// Answers a request: Zmail's page, its inline parts and the remote
    /// images Zmail fetches, or an empty 403 for anything else.
    fn resource_requested(
        self: &Rc<Self>,
        environment: &ICoreWebView2Environment10,
        args: &ICoreWebView2WebResourceRequestedEventArgs,
    ) -> windows::core::Result<()> {
        let mut uri = PWSTR::null();
        // SAFETY: plain property reads on the arguments WebView2 gave us;
        // `take_pwstr` frees the string it allocated.
        let uri = unsafe {
            args.Request()?.Uri(&mut uri)?;
            take_pwstr(uri)
        };
        let page = &self.page;
        let served = match parse_scheme_request(&uri) {
            SchemeRequest::Page(token) if token == page.token => Some((
                "text/html; charset=utf-8".to_owned(),
                page.html.as_slice(),
                Some(page.policy.as_str()),
            )),
            SchemeRequest::Inline(content_id) => page
                .resources
                .get(&content_id)
                // Inline parts are only ever images.
                .filter(|resource| resource.mime_type.starts_with("image/"))
                .map(|resource| (resource.mime_type.clone(), &resource.data[..], None)),
            SchemeRequest::Remote(index) => {
                if let Some(Some(remote)) = page.remote.get(index) {
                    debug_log(format_args!("fetching remote image {index}"));
                    // SAFETY: a plain call on the arguments WebView2 gave
                    // us; the deferral is completed in `answer_fetches`.
                    let deferral = unsafe { args.GetDeferral()? };
                    self.pending.borrow_mut().push(PendingFetch {
                        args: args.clone(),
                        deferral,
                        result: RemoteFetcher::shared().fetch(remote.clone()),
                    });
                    self.start_polling();
                    return Ok(());
                }
                None
            }
            _ => None,
        };
        let response = match served {
            Some((mime_type, body, policy)) => {
                response(environment, 200, "OK", &mime_type, body, policy).or_else(|error| {
                    eprintln!("zmail: web renderer: no response for {uri}: {error}");
                    blocked(environment)
                })
            }
            None => {
                debug_log(format_args!("request refused: {uri}"));
                blocked(environment)
            }
        }?;
        // SAFETY: a plain property write on the arguments WebView2 gave us,
        // with a response it created. Without a response a web request
        // would go to the network, so every path above ends in one, or in
        // an error.
        unsafe { args.SetResponse(&response) }
    }

    /// Checks the remote image fetches on a timer, unless already checking.
    fn start_polling(self: &Rc<Self>) {
        if self.polling.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        after(FETCH_POLL, move || {
            if let Some(state) = weak.upgrade() {
                state.polling.set(false);
                if !state.answer_fetches() {
                    state.start_polling();
                }
            }
        });
    }

    /// Answers the requests whose fetch finished. Returns whether none is
    /// left.
    fn answer_fetches(&self) -> bool {
        let finished: Vec<(PendingFetch, FetchResult)> = {
            let mut pending = self.pending.borrow_mut();
            let mut finished = Vec::new();
            let mut waiting = Vec::new();
            for fetch in pending.drain(..) {
                match fetch.result.try_recv() {
                    Ok(result) => finished.push((fetch, result)),
                    Err(TryRecvError::Empty) => waiting.push(fetch),
                    Err(TryRecvError::Disconnected) => finished.push((
                        fetch,
                        Err(RemoteError::Request("the fetcher stopped".to_owned())),
                    )),
                }
            }
            *pending = waiting;
            finished
        };
        let web = self.web.borrow();
        let Some(environment) = web.as_ref().map(|web| &web.environment) else {
            return true;
        };
        for (fetch, result) in finished {
            let response = match result {
                Ok(image) => {
                    debug_log(format_args!(
                        "remote image fetched: {} bytes of {}",
                        image.bytes.len(),
                        image.content_type
                    ));
                    response(
                        environment,
                        200,
                        "OK",
                        &image.content_type,
                        &image.bytes,
                        None,
                    )
                }
                Err(error) => {
                    debug_log(format_args!("remote image refused: {error}"));
                    blocked(environment)
                }
            };
            // SAFETY: the deferred arguments are live until the deferral is
            // completed, which happens once, here. (A `zmail-mail:` request
            // left without a response fails; it never reaches the network.)
            unsafe {
                if let Ok(response) = response {
                    let _ = fetch.args.SetResponse(&response);
                }
                let _ = fetch.deferral.Complete();
            }
        }
        drop(web);
        self.pending.borrow().is_empty()
    }

    fn process_failed(
        &self,
        args: &ICoreWebView2ProcessFailedEventArgs,
    ) -> windows::core::Result<()> {
        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED;
        // SAFETY: a plain property read on the arguments WebView2 gave us.
        unsafe { args.ProcessFailedKind(&mut kind)? };
        eprintln!(
            "zmail: web renderer: a WebView2 process failed (kind {})",
            kind.0
        );
        if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
            || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
            || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE
        {
            (self.on_event)(WebEvent::ContentProcessTerminated);
        }
        Ok(())
    }

    /// Runs one of Zmail's scripts and hands its JSON result to `then`.
    fn run_script(&self, script: &str, then: impl FnOnce(Result<String, String>) + 'static) {
        let web = self.web.borrow();
        let Some(web) = web.as_ref() else {
            then(Err("the web view does not exist yet".to_owned()));
            return;
        };
        let then = RefCell::new(Some(then));
        let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
            if let Some(then) = then.borrow_mut().take() {
                then(result.map(|()| json).map_err(|error| error.to_string()));
            }
            Ok(())
        }));
        // SAFETY: a plain call on the live web view with a script string
        // that lives through the call; WebView2 calls the handler once.
        if let Err(error) = unsafe { web.webview.ExecuteScript(&HSTRING::from(script), &handler) } {
            eprintln!("zmail: web renderer: a script did not run: {error}");
        }
    }

    /// Measures the page and reports its height.
    fn measure(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.run_script(MEASURE_SCRIPT, move |result| {
            let Some(state) = weak.upgrade() else {
                return;
            };
            match result {
                Ok(json) => state.measured(&json),
                Err(error) => {
                    eprintln!("zmail: web renderer: measuring the message failed: {error}");
                    (state.on_event)(WebEvent::LoadFailed(format!("measuring failed: {error}")));
                }
            }
        });
    }

    fn measured(self: &Rc<Self>, json: &str) {
        let Some(measurement) = decode_json_string(json)
            .as_deref()
            .and_then(Measurement::parse)
        else {
            eprintln!("zmail: web renderer: unexpected measurement {json:?}");
            (self.on_event)(WebEvent::LoadFailed("unexpected measurement".to_owned()));
            return;
        };
        let zoom = self.zoom.get();
        // WebView2 lays the page out again asynchronously after a resize
        // or a zoom: measure again until it reports the new viewport.
        let expected = (f64::from(self.size.get().0) / self.scale.get() / zoom).floor();
        if (measurement.viewport_width - expected).abs() > 2.
            && self.relayout_waits.get() < MAX_RELAYOUT_WAITS
        {
            self.relayout_waits.set(self.relayout_waits.get() + 1);
            let weak = Rc::downgrade(self);
            after(RELAYOUT_WAIT, move || {
                if let Some(state) = weak.upgrade() {
                    state.measure();
                }
            });
            return;
        }
        self.relayout_waits.set(0);
        if self.fit_pending.replace(false)
            && let Some(fit) = measurement.fit_zoom()
        {
            // Wider than the pane: zoom out to fit, then measure again.
            debug_log(format_args!(
                "page {}px wide in a {}px pane: zoom {fit:.2}",
                measurement.scroll_width, measurement.viewport_width
            ));
            self.set_zoom(fit);
            self.measure();
            return;
        }
        let height = measurement.height;
        if height * zoom > MAX_HEIGHT {
            eprintln!("zmail: web renderer: message cut off at {MAX_HEIGHT} points");
        }
        debug_log(format_args!("measured {height}px at zoom {zoom:.2}"));
        (self.on_event)(WebEvent::Measured {
            height: measurement.points(zoom) as f32,
        });
    }

    fn set_zoom(&self, zoom: f64) {
        self.zoom.set(zoom);
        if let Some(web) = &*self.web.borrow() {
            // SAFETY: a plain property write on a live controller.
            let _ = unsafe { web.controller.SetZoomFactor(zoom) };
        }
    }

    /// The view's width changed: fit and measure again.
    fn refit(self: &Rc<Self>) {
        let width = self.size.get().0;
        if !self.loaded.get() || self.fitted_width.replace(width) == width {
            return;
        }
        self.fit_pending.set(true);
        self.set_zoom(1.);
        self.measure();
    }

    fn apply_bounds(self: &Rc<Self>) {
        let (width, height) = self.size.get();
        if let Some(web) = &*self.web.borrow() {
            // SAFETY: a plain property write on a live controller; the
            // origin is the visual's, which the host places.
            let _ = unsafe {
                web.controller.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                })
            };
        }
        self.refit();
    }
}

impl Drop for ViewState {
    fn drop(&mut self) {
        let pending = std::mem::take(self.pending.get_mut());
        if let Some(web) = self.web.get_mut().take() {
            // SAFETY: refuses each waiting request once and completes its
            // deferral, then disconnects the live view from its visual and
            // closes it; WebView2 releases our handlers when it closes.
            unsafe {
                for fetch in pending {
                    if let Ok(response) = blocked(&web.environment) {
                        let _ = fetch.args.SetResponse(&response);
                    }
                    let _ = fetch.deferral.Complete();
                }
                let _ = web.composition.SetRootVisualTarget(None);
                let _ = web.controller.Close();
            }
        }
    }
}

/// A response with Zmail's headers.
fn response(
    environment: &ICoreWebView2Environment10,
    status: i32,
    reason: &str,
    mime_type: &str,
    body: &[u8],
    policy: Option<&str>,
) -> windows::core::Result<ICoreWebView2WebResourceResponse> {
    let mut headers = format!(
        "Content-Type: {mime_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer",
        body.len()
    );
    if let Some(policy) = policy {
        headers.push_str("\r\nContent-Security-Policy: ");
        headers.push_str(policy);
    }
    // SAFETY: SHCreateMemStream copies `body`; the strings live through the
    // call.
    unsafe {
        let stream: Option<IStream> = SHCreateMemStream(Some(body));
        environment.CreateWebResourceResponse(
            stream.as_ref(),
            status,
            &HSTRING::from(reason),
            &HSTRING::from(headers),
        )
    }
}

/// The empty 403 every refused request gets.
fn blocked(
    environment: &ICoreWebView2Environment10,
) -> windows::core::Result<ICoreWebView2WebResourceResponse> {
    response(environment, 403, "Blocked", "text/plain", &[], None)
}

/// Gives keyboard focus back to the window that hosts the views.
fn focus_window(window: HWND) {
    // SAFETY: sets the focus to a live window of this thread.
    let _ = unsafe { SetFocus(Some(window)) };
}

/// Every hardening setting of ADR 0006 §3 that WebView2 has, and the
/// view's own profile settings.
fn harden(webview: &ICoreWebView2, dark: bool) -> Result<()> {
    // SAFETY: plain property writes on the live web view's settings and
    // profile, before any page loads.
    unsafe {
        let settings = webview.Settings()?;
        settings.SetIsScriptEnabled(false)?;
        settings.SetAreDefaultScriptDialogsEnabled(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsZoomControlEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsBuiltInErrorPageEnabled(false)?;
        let settings: ICoreWebView2Settings8 = settings
            .cast()
            .context("the WebView2 runtime is too old (no ICoreWebView2Settings8)")?;
        settings.SetAreBrowserAcceleratorKeysEnabled(false)?;
        settings.SetIsPasswordAutosaveEnabled(false)?;
        settings.SetIsGeneralAutofillEnabled(false)?;
        settings.SetIsPinchZoomEnabled(false)?;
        settings.SetIsSwipeNavigationEnabled(false)?;
        // SmartScreen would look each URL up with Microsoft.
        settings.SetIsReputationCheckingRequired(false)?;

        let webview13: ICoreWebView2_13 = webview
            .cast()
            .context("the WebView2 runtime is too old (no ICoreWebView2_13)")?;
        let profile = webview13.Profile()?;
        profile.SetPreferredColorScheme(if dark {
            COREWEBVIEW2_PREFERRED_COLOR_SCHEME_DARK
        } else {
            COREWEBVIEW2_PREFERRED_COLOR_SCHEME_LIGHT
        })?;
        let profile3: ICoreWebView2Profile3 = profile
            .cast()
            .context("the WebView2 runtime is too old (no ICoreWebView2Profile3)")?;
        profile3
            .SetPreferredTrackingPreventionLevel(COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT)?;
        if let Ok(webview8) = webview.cast::<ICoreWebView2_8>() {
            webview8.SetIsMuted(true)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The clipboard
// ---------------------------------------------------------------------------

/// Puts `text` on the clipboard, owned by `window`.
fn set_clipboard_text(window: HWND, text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // SAFETY: the standard clipboard sequence on this thread: the clipboard
    // is opened, emptied, given a movable global block holding the
    // NUL-terminated UTF-16 text (written while locked, within its size),
    // and closed again on every path. Once SetClipboardData succeeds the
    // clipboard owns the block; before that it is freed here.
    unsafe {
        OpenClipboard(Some(window)).context("opening the clipboard")?;
        let result = (|| -> Result<()> {
            EmptyClipboard()?;
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let target = GlobalLock(memory).cast::<u16>();
            if target.is_null() {
                let _ = GlobalFree(Some(memory));
                bail!("could not lock the clipboard memory");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
            let _ = GlobalUnlock(memory);
            if let Err(error) =
                SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0)))
            {
                let _ = GlobalFree(Some(memory));
                return Err(error.into());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

/// The clipboard's text, if it has any. For the canary test.
pub fn clipboard_text() -> Option<String> {
    // SAFETY: the clipboard is opened and closed on this thread; the data
    // handle is only read while the clipboard is open and the block locked,
    // up to its NUL terminator.
    unsafe {
        OpenClipboard(None).ok()?;
        let text = (|| {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
            let memory = HGLOBAL(handle.0);
            let source = GlobalLock(memory).cast::<u16>();
            if source.is_null() {
                return None;
            }
            let mut length = 0;
            while *source.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(source, length));
            let _ = GlobalUnlock(memory);
            Some(text)
        })();
        let _ = CloseClipboard();
        text
    }
}

// ---------------------------------------------------------------------------
// Test support: a window with its own compositor
// ---------------------------------------------------------------------------

/// A borderless window with a DirectComposition tree, for running the real
/// web views in tests without GPUI.
pub struct TestWindow {
    window: HWND,
    device: IDCompositionDevice,
    _target: IDCompositionTarget,
    root: IDCompositionVisual,
}

impl TestWindow {
    /// A window of `width` x `height` pixels at the top left of the screen.
    pub fn new(width: i32, height: i32) -> Result<Self> {
        // SAFETY: OLE on this thread makes it the single-threaded apartment
        // WebView2 needs. The window class's procedure has the WNDPROC
        // signature and defers to DefWindowProcW; the class name and title
        // are static strings. The window and the composition objects are
        // owned by the returned value.
        unsafe {
            let _ = OleInitialize(None);
            let instance = GetModuleHandleW(None)?;
            let class = w!("ZmailWebCanary");
            let class_info = WNDCLASSW {
                lpfnWndProc: Some(test_window_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            // Registering twice fails harmlessly; creating the window tells.
            RegisterClassW(&class_info);
            let window = CreateWindowExW(
                WS_EX_NOACTIVATE,
                class,
                w!("Zmail web canary"),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                width,
                height,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .context("creating the test window")?;
            let device: IDCompositionDevice =
                DCompositionCreateDevice2(None).context("creating a compositor")?;
            let target = device.CreateTargetForHwnd(window, true)?;
            let root = device.CreateVisual()?;
            target.SetRoot(&root)?;
            device.Commit()?;
            Ok(Self {
                window,
                device,
                _target: target,
                root,
            })
        }
    }

    /// A new visual in the window for a web view to draw into.
    pub fn target(&self) -> Result<VisualTarget> {
        // SAFETY: plain calls on the window's live compositor.
        unsafe {
            let visual = self.device.CreateVisual()?;
            self.root.AddVisual(&visual, false, None)?;
            self.device.Commit()?;
            Ok(VisualTarget {
                window: self.window,
                visual: visual.cast()?,
            })
        }
    }

    /// Runs this thread's message loop for `duration`, so WebView2's
    /// callbacks and Zmail's timers arrive, committing the compositor as it
    /// goes (WebView2 leaves that to its host).
    pub fn pump(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        let mut message = MSG::default();
        while Instant::now() < deadline {
            // SAFETY: the standard message loop on this thread, with a
            // message struct that outlives each call; a commit on the live
            // compositor.
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                let _ = self.device.Commit();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Whether DirectComposition adds a visual to a parent it is already
    /// in (it does not), which is why the host detaches the message
    /// surfaces before GPUI rebuilds its tree (`detach_children`).
    pub fn readding_a_child_fails(&self) -> Result<bool> {
        // SAFETY: plain calls on the window's live compositor, on visuals
        // made here and dropped at the end.
        unsafe {
            let parent = self.device.CreateVisual()?;
            let child = self.device.CreateVisual()?;
            parent.AddVisual(&child, true, None)?;
            let again = parent.AddVisual(&child, true, None);
            if let Err(error) = &again {
                println!("canary: DirectComposition refuses to re-add a child visual: {error}");
            }
            Ok(again.is_err())
        }
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: destroys the window this value created, on its thread.
        let _ = unsafe { DestroyWindow(self.window) };
    }
}

unsafe extern "system" fn test_window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: passes the message Windows gave us to the default procedure.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
