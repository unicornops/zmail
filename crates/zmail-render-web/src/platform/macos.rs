//! The macOS web renderer: a locked-down `WKWebView` per message.
//!
//! This is the only module of Zmail that uses `unsafe`: it talks to AppKit
//! and WebKit through `objc2`. Every `unsafe` block says why it is sound.
//! The rest of the crate sees a small safe API: a [`Clip`] (the view that
//! holds and clips a reader's web views inside GPUI's native composition
//! surface) and a [`PlatformWebView`] per message.
//!
//! Each web view is hardened as ADR 0006 §3 requires:
//!
//! - JavaScript is off for page content (`allowsContentJavaScript = NO`)
//!   and Lockdown Mode is on where the OS has it (macOS 13+: no JIT, WebGL,
//!   remote fonts, ...). Zmail's own measuring script runs in WebKit's
//!   isolated client world, which page content cannot see.
//! - The page comes from Zmail's `zmail-mail:` scheme handler with a CSP
//!   response header (and a `<meta>` copy); `cid:` parts come from the same
//!   handler, and so do remote images the user allowed, which Zmail fetches
//!   itself (`crate::remote`). The handler serves nothing else.
//! - A `WKContentRuleList` blocks every load that is not Zmail's scheme or
//!   a `data:`/`cid:` image, so the engine never connects anywhere. The
//!   view is not created until the list is compiled.
//! - The navigation delegate allows exactly one navigation: the page load
//!   Zmail starts. Link clicks are cancelled and reported to Zmail, which
//!   asks before opening anything. No new windows, downloads, file panels,
//!   link previews, magnification, back/forward gestures or drag and drop;
//!   the context menu keeps only Copy.
//! - Each view has its own non-persistent website data store.
//!
//! The view never becomes first responder, so keyboard shortcuts stay with
//! GPUI; vertical scrolling is passed to GPUI's view, so the conversation
//! scrolls as one list (the page itself is sized to its content and does
//! not scroll vertically).

#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    ptr::NonNull,
    rc::{Rc, Weak as RcWeak},
    sync::mpsc::{Receiver, TryRecvError},
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use block2::{DynBlock, RcBlock};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send,
    rc::{Retained, Weak},
    runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject},
    sel,
};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSEvent, NSMenu, NSResponder, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSData, NSDate, NSDefaultRunLoopMode, NSDictionary, NSError, NSHTTPURLResponse,
    NSInteger, NSPoint, NSRect, NSRunLoop, NSSize, NSString, NSTimer, NSUInteger, NSURL,
    NSURLErrorDomain, NSURLRequest,
};
use objc2_web_kit::{
    WKAudiovisualMediaTypes, WKContentRuleList, WKContentRuleListStore, WKContentWorld,
    WKFrameInfo, WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKNavigationType, WKOpenPanelParameters,
    WKUIDelegate, WKURLSchemeHandler, WKURLSchemeTask, WKWebView, WKWebViewConfiguration,
    WKWebsiteDataStore, WKWindowFeatures,
};

use crate::document::{
    CID_SCHEME, InlineResource, MAIL_SCHEME, SchemeRequest, WebDocument, content_security_policy,
    page_html, page_url, parse_scheme_request,
};
use crate::remote::{FetchResult, RemoteFetcher};
use crate::rules::{content_rule_list, rule_list_identifier};
use crate::{WebEvent, debug_log};

/// Zmail's height measurement, run in WebKit's isolated client world: the
/// page's content height and scroll width, and the viewport width, all in
/// CSS pixels.
const MEASURE_SCRIPT: &str = "(() => { \
    const d = document.documentElement, b = document.body; \
    let h = d.getBoundingClientRect().height; \
    if (b) { \
        const s = getComputedStyle(b); \
        h = Math.max(h, b.getBoundingClientRect().bottom + (parseFloat(s.marginBottom) || 0)); \
    } \
    const w = Math.max(d.scrollWidth, b ? b.scrollWidth : 0); \
    return Math.ceil(h) + ' ' + Math.ceil(w) + ' ' + Math.floor(window.innerWidth); \
})()";

/// The smallest zoom used to fit a page that is wider than the pane.
const MIN_FIT_ZOOM: f64 = 0.5;

/// The tallest a page is shown, in points; taller mail is cut off.
const MAX_HEIGHT: f64 = 40_000.;

/// The error code the scheme handler fails unknown requests with
/// (`NSURLErrorFileDoesNotExist`).
const NOT_FOUND: NSInteger = -1100;

/// `NSURLErrorCancelled`, which every cancelled navigation reports.
const CANCELLED: NSInteger = -999;

/// `WebKitErrorFrameLoadInterruptedByPolicyChange`.
const INTERRUPTED_BY_POLICY: NSInteger = 102;

fn main_thread() -> Result<MainThreadMarker> {
    MainThreadMarker::new().context("the web renderer must run on the main thread")
}

// ---------------------------------------------------------------------------
// The clip view
// ---------------------------------------------------------------------------

define_class!(
    // SAFETY: NSView has no subclassing requirements we do not meet: the
    // class overrides only `isFlipped` and `hitTest:` with their AppKit
    // signatures, and does not implement `Drop`.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZmailMailClipView"]
    struct ClipView;

    impl ClipView {
        /// Top-left origin, like GPUI's coordinates.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// Transparent to the mouse where there is no web view, so clicks
        /// between messages go on to GPUI.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            // SAFETY: calls NSView's own `hitTest:` with the argument AppKit
            // gave us; it returns a view or nil.
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            hit.filter(|view| !std::ptr::eq::<NSView>(&**view, &**self))
        }
    }
);

impl ClipView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `initWithFrame:` is NSView's designated initializer and
        // ClipView adds no ivars that need initializing.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// The view a reader's web views live in, filling GPUI's native
/// composition surface for the reader. The surface's view clips to its
/// bounds (the conversation list's viewport), so web views scrolled partly
/// out of the list are cut off at its edges.
pub struct Clip {
    view: Retained<ClipView>,
    /// The surface's own view, kept alive as long as we use it.
    _container: Retained<NSView>,
    /// GPUI's view for the window: scrolling over a web view goes there.
    gpui_view: Weak<NSView>,
}

impl Clip {
    /// Fills `container` (the address of the `NSView` GPUI created for a
    /// native composition surface, from its platform handle) with a clip
    /// view. `gpui_view` is the window's GPUI `NSView` (from its raw window
    /// handle).
    pub(crate) fn attach(container: usize, gpui_view: NonNull<c_void>) -> Result<Self> {
        let mtm = main_thread()?;
        // SAFETY: GPUI's macOS backend documents a native surface's platform
        // handle as the address of the AppKit view it created for the
        // surface (`MacNativeSurface::platform_handle`), and the view lives
        // as long as the surface, which the caller holds while calling us.
        // Retaining it keeps it valid for as long as this Clip uses it.
        let container: Retained<NSView> = unsafe { Retained::retain(container as *mut NSView) }
            .context("the composition surface has no view")?;
        // SAFETY: the raw window handle GPUI gives for a window on macOS is
        // an `AppKitWindowHandle` whose `ns_view` is the window's live GPUI
        // view; we only keep a weak reference to it.
        let gpui_view: Retained<NSView> =
            unsafe { Retained::retain(gpui_view.as_ptr().cast::<NSView>()) }
                .context("the window has no view")?;
        let view = ClipView::new(mtm, container.bounds());
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        container.addSubview(&view);
        debug_log(format_args!(
            "clip view attached ({:.0}x{:.0})",
            container.bounds().size.width,
            container.bounds().size.height
        ));
        Ok(Self {
            view,
            _container: container,
            gpui_view: Weak::from_retained(&gpui_view),
        })
    }

    /// Shows or hides every web view in the clip.
    pub(crate) fn set_visible(&self, visible: bool) {
        self.view.setHidden(!visible);
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        self.view.removeFromSuperview();
    }
}

// ---------------------------------------------------------------------------
// The web view
// ---------------------------------------------------------------------------

/// What a [`MailWebView`] carries.
struct MailWebViewIvars {
    gpui_view: Weak<NSView>,
    on_event: Rc<dyn Fn(WebEvent)>,
    /// The page finished loading, so it can be measured.
    loaded: Cell<bool>,
    /// The next measurement may zoom the page out to fit its width.
    fit_pending: Cell<bool>,
    /// The width the page was last fitted at.
    fitted_width: Cell<f64>,
}

define_class!(
    // SAFETY: WKWebView may be subclassed. The overrides keep AppKit's
    // signatures, forward to the superclass where the event should still
    // reach WebKit, and the class does not implement `Drop`.
    #[unsafe(super(WKWebView, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZmailMailWebView"]
    #[ivars = MailWebViewIvars]
    struct MailWebView;

    impl MailWebView {
        /// Keyboard focus stays with GPUI, so Zmail's shortcuts keep
        /// working after a click in a message. Copy and Select All reach
        /// the page through Zmail's own commands.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            (self.ivars().on_event)(WebEvent::Pressed);
            // SAFETY: forwards the event AppKit gave us to WKWebView's own
            // `mouseDown:`, so selection and link clicks still work.
            unsafe { msg_send![super(self), mouseDown: event] }
        }

        /// Vertical scrolling scrolls the conversation (GPUI); sideways
        /// scrolling stays with a page wider than the pane.
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            let vertical = event.scrollingDeltaY().abs() >= event.scrollingDeltaX().abs();
            if vertical && let Some(gpui_view) = self.ivars().gpui_view.load() {
                gpui_view.scrollWheel(event);
                return;
            }
            // SAFETY: forwards the event to WKWebView's own `scrollWheel:`.
            unsafe { msg_send![super(self), scrollWheel: event] }
        }

        /// The context menu keeps only Copy: no Open Link, Open Image,
        /// Download, Look Up, Share, Services or Reload.
        #[unsafe(method(willOpenMenu:withEvent:))]
        fn will_open_menu(&self, menu: &NSMenu, event: &NSEvent) {
            for item in menu.itemArray().iter() {
                if item.action() != Some(sel!(copy:)) {
                    menu.removeItem(&item);
                }
            }
            // SAFETY: lets WKWebView finish its own bookkeeping for the menu
            // it is about to show, with the arguments AppKit gave us.
            unsafe { msg_send![super(self), willOpenMenu: menu, withEvent: event] }
        }

        /// Nothing can be dragged out of a message.
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn drag_source_operations(&self, _session: &AnyObject, _context: NSInteger) -> NSUInteger {
            0
        }

        /// Nothing can be dropped onto a message (a dropped file would
        /// otherwise be opened in the view).
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _info: &AnyObject) -> NSUInteger {
            0
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, _info: &AnyObject) -> NSUInteger {
            0
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag(&self, _info: &AnyObject) -> bool {
            false
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, _info: &AnyObject) -> bool {
            false
        }
    }
);

impl MailWebView {
    fn new(
        mtm: MainThreadMarker,
        frame: NSRect,
        configuration: &WKWebViewConfiguration,
        ivars: MailWebViewIvars,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `initWithFrame:configuration:` is WKWebView's designated
        // initializer; the ivars were set above.
        unsafe { msg_send![super(this), initWithFrame: frame, configuration: configuration] }
    }

    fn emit(&self, event: WebEvent) {
        (self.ivars().on_event)(event);
    }

    /// Measures the page in the isolated world and reports its height.
    fn measure(&self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let weak = Weak::new(self);
        let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            let Some(view) = weak.load() else {
                return;
            };
            // SAFETY: WebKit passes the script's result (or nil) and an
            // error (or nil), both valid for the duration of the callback.
            let (result, error) = unsafe { (result.as_ref(), error.as_ref()) };
            if let Some(error) = error {
                let message = error.localizedDescription().to_string();
                eprintln!("zmail: web renderer: measuring the message failed: {message}");
                view.emit(WebEvent::LoadFailed(format!("measuring failed: {message}")));
                return;
            }
            let text = result
                .and_then(|result| result.downcast_ref::<NSString>())
                .map(NSString::to_string)
                .unwrap_or_default();
            view.measured(&text);
        });
        let script = NSString::from_str(MEASURE_SCRIPT);
        // SAFETY: the default client world is WebKit's isolated world for
        // app scripts; the frame argument may be nil (the main frame).
        unsafe {
            let world = WKContentWorld::defaultClientWorld(mtm);
            self.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &script,
                None,
                &world,
                Some(&block),
            );
        }
    }

    /// A measurement came back as "height scrollWidth viewportWidth".
    fn measured(&self, text: &str) {
        let numbers: Vec<f64> = text
            .split_whitespace()
            .filter_map(|part| part.parse().ok())
            .collect();
        let [height, scroll_width, viewport_width] = numbers[..] else {
            eprintln!("zmail: web renderer: unexpected measurement {text:?}");
            self.emit(WebEvent::LoadFailed("unexpected measurement".to_owned()));
            return;
        };
        // SAFETY: plain property reads and writes on a live web view.
        let zoom = unsafe { self.pageZoom() };
        let ivars = self.ivars();
        if ivars.fit_pending.replace(false)
            && viewport_width > 0.
            && scroll_width > viewport_width + 1.
        {
            // Wider than the pane: zoom out to fit, then measure again.
            let fit = (viewport_width / scroll_width).clamp(MIN_FIT_ZOOM, 1.);
            debug_log(format_args!(
                "page {scroll_width}px wide in a {viewport_width}px pane: zoom {fit:.2}"
            ));
            // SAFETY: as above.
            unsafe { self.setPageZoom(fit) };
            self.measure();
            return;
        }
        let points = (height * zoom).ceil().clamp(1., MAX_HEIGHT);
        if height * zoom > MAX_HEIGHT {
            eprintln!("zmail: web renderer: message cut off at {MAX_HEIGHT} points");
        }
        debug_log(format_args!("measured {height}px at zoom {zoom:.2}"));
        self.emit(WebEvent::Measured {
            height: points as f32,
        });
    }

    /// The view's width changed: fit and measure again.
    fn refit(&self, width: f64) {
        let ivars = self.ivars();
        if !ivars.loaded.get() || (ivars.fitted_width.get() - width).abs() < 0.5 {
            return;
        }
        ivars.fitted_width.set(width);
        ivars.fit_pending.set(true);
        // SAFETY: plain property write on a live web view.
        unsafe { self.setPageZoom(1.) };
        self.measure();
    }
}

// ---------------------------------------------------------------------------
// The delegate: navigation policy, UI policy and the scheme handler
// ---------------------------------------------------------------------------

/// The page a delegate serves.
struct ServedPage {
    url: String,
    html: Vec<u8>,
    policy: String,
    resources: HashMap<String, InlineResource>,
    /// Remote images Zmail may fetch, by the `n` of
    /// `zmail-mail://remote/<n>`.
    remote: Vec<Option<String>>,
}

/// A remote image being fetched for a scheme task.
struct PendingFetch {
    task: Retained<ProtocolObject<dyn WKURLSchemeTask>>,
    request: Retained<NSURLRequest>,
    result: Receiver<FetchResult>,
}

struct DelegateIvars {
    on_event: Rc<dyn Fn(WebEvent)>,
    page: ServedPage,
    view: RefCell<Weak<MailWebView>>,
    /// Remote image fetches whose scheme tasks are waiting.
    pending: RefCell<Vec<PendingFetch>>,
    /// Polls `pending` on the main run loop while it is not empty.
    poll: RefCell<Option<Retained<NSTimer>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the class does not
    // implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ZmailMailWebDelegate"]
    #[ivars = DelegateIvars]
    struct WebDelegate;

    // SAFETY: NSObjectProtocol has no extra requirements.
    unsafe impl NSObjectProtocol for WebDelegate {}

    // SAFETY: the methods match WKNavigationDelegate's signatures, and every
    // decision handler is called exactly once.
    unsafe impl WKNavigationDelegate for WebDelegate {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_navigation_action(
            &self,
            _web_view: &WKWebView,
            action: &WKNavigationAction,
            decision_handler: &DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: plain property reads on the action WebKit gave us.
            let (url, kind, main_frame) = unsafe {
                let url = request_url(&action.request());
                let main_frame = action
                    .targetFrame()
                    .is_some_and(|frame| frame.isMainFrame());
                (url, action.navigationType(), main_frame)
            };
            let ivars = self.ivars();
            let allowed = main_frame && kind == WKNavigationType::Other && url == ivars.page.url;
            if kind == WKNavigationType::LinkActivated {
                (ivars.on_event)(WebEvent::LinkActivated { url: url.clone() });
            }
            if !allowed {
                debug_log(format_args!("navigation denied: {url} ({kind:?})"));
            }
            decision_handler.call((if allowed {
                WKNavigationActionPolicy::Allow
            } else {
                WKNavigationActionPolicy::Cancel
            },));
        }

        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_navigation_response(
            &self,
            _web_view: &WKWebView,
            response: &WKNavigationResponse,
            decision_handler: &DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // SAFETY: plain property reads on the response WebKit gave us.
            let (url, main_frame) = unsafe {
                let url = response
                    .response()
                    .URL()
                    .and_then(|url| url.absoluteString())
                    .map(|url| url.to_string())
                    .unwrap_or_default();
                (url, response.isForMainFrame())
            };
            let allowed = main_frame && url == self.ivars().page.url;
            decision_handler.call((if allowed {
                WKNavigationResponsePolicy::Allow
            } else {
                WKNavigationResponsePolicy::Cancel
            },));
        }

        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish_navigation(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            debug_log(format_args!("page loaded"));
            if let Some(view) = self.ivars().view.borrow().load() {
                view.ivars().loaded.set(true);
                view.ivars().fit_pending.set(true);
                view.ivars().fitted_width.set(view.frame().size.width);
                view.measure();
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail_navigation(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.report_failure(error);
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional_navigation(
            &self,
            _web_view: &WKWebView,
            _navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            self.report_failure(error);
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn content_process_did_terminate(&self, _web_view: &WKWebView) {
            eprintln!("zmail: web renderer: the web content process ended");
            (self.ivars().on_event)(WebEvent::ContentProcessTerminated);
        }
    }

    // SAFETY: the methods match WKUIDelegate's signatures, and the file
    // panel's completion handler is called exactly once.
    unsafe impl WKUIDelegate for WebDelegate {
        /// No new windows, ever.
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        fn create_web_view(
            &self,
            _web_view: &WKWebView,
            _configuration: &WKWebViewConfiguration,
            _action: &WKNavigationAction,
            _features: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            None
        }

        /// No file panels.
        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        fn run_open_panel(
            &self,
            _web_view: &WKWebView,
            _parameters: &WKOpenPanelParameters,
            _frame: &WKFrameInfo,
            completion_handler: &DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
        ) {
            completion_handler.call((std::ptr::null_mut(),));
        }
    }

    // SAFETY: the methods match WKURLSchemeHandler's signatures; every task
    // is answered synchronously, before `webView:stopURLSchemeTask:` can be
    // called for it.
    unsafe impl WKURLSchemeHandler for WebDelegate {
        #[unsafe(method(webView:startURLSchemeTask:))]
        fn start_task(&self, _web_view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            // SAFETY: plain property read on the task WebKit gave us.
            let request = unsafe { task.request() };
            let url = request_url(&request);
            let page = &self.ivars().page;
            let served = match parse_scheme_request(&url) {
                SchemeRequest::Page(_) if url == page.url => Some((
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
                        self.ivars().pending.borrow_mut().push(PendingFetch {
                            task: task.retain(),
                            request: request.clone(),
                            result: RemoteFetcher::shared().fetch(remote.clone()),
                        });
                        self.start_polling();
                        return;
                    }
                    None
                }
                _ => None,
            };
            match served {
                Some((mime_type, body, policy)) => {
                    respond(task, &request, &mime_type, body, policy)
                }
                None => {
                    debug_log(format_args!("scheme request refused: {url}"));
                    fail(task);
                }
            }
        }

        /// WebKit no longer wants a task's answer: a remote image still
        /// being fetched is dropped, and never answered.
        #[unsafe(method(webView:stopURLSchemeTask:))]
        fn stop_task(&self, _web_view: &WKWebView, task: &ProtocolObject<dyn WKURLSchemeTask>) {
            self.ivars()
                .pending
                .borrow_mut()
                .retain(|pending| !std::ptr::eq(&*pending.task, task));
        }
    }
);

impl WebDelegate {
    fn new(mtm: MainThreadMarker, ivars: DelegateIvars) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ivars);
        // SAFETY: `init` is NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }

    /// Starts polling the remote image fetches, unless already polling.
    fn start_polling(&self) {
        let mut poll = self.ivars().poll.borrow_mut();
        if poll.as_ref().is_some_and(|timer| timer.isValid()) {
            return;
        }
        let delegate = Weak::new(self);
        let block = RcBlock::new(move |timer: NonNull<NSTimer>| {
            let done = match delegate.load() {
                Some(delegate) => delegate.answer_fetches(),
                None => true,
            };
            if done {
                // SAFETY: NSTimer passes itself, live for the callback.
                unsafe { timer.as_ref() }.invalidate();
            }
        });
        // SAFETY: the timer is scheduled on the current (main) run loop, so
        // the block, which captures main-thread-only objects, only ever runs
        // on the main thread.
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.03, true, &block) };
        *poll = Some(timer);
    }

    /// Answers the scheme tasks whose fetch finished. Returns whether none
    /// is left.
    fn answer_fetches(&self) -> bool {
        let finished: Vec<(PendingFetch, FetchResult)> = {
            let mut pending = self.ivars().pending.borrow_mut();
            let mut finished = Vec::new();
            let mut waiting = Vec::new();
            for fetch in pending.drain(..) {
                match fetch.result.try_recv() {
                    Ok(result) => finished.push((fetch, result)),
                    Err(TryRecvError::Empty) => waiting.push(fetch),
                    Err(TryRecvError::Disconnected) => finished.push((
                        fetch,
                        Err(crate::remote::RemoteError::Request(
                            "the fetcher stopped".to_owned(),
                        )),
                    )),
                }
            }
            *pending = waiting;
            finished
        };
        for (fetch, result) in finished {
            match result {
                Ok(image) => {
                    debug_log(format_args!(
                        "remote image fetched: {} bytes of {}",
                        image.bytes.len(),
                        image.content_type
                    ));
                    respond(
                        &fetch.task,
                        &fetch.request,
                        &image.content_type,
                        &image.bytes,
                        None,
                    );
                }
                Err(error) => {
                    debug_log(format_args!("remote image refused: {error}"));
                    fail(&fetch.task);
                }
            }
        }
        self.ivars().pending.borrow().is_empty()
    }

    fn report_failure(&self, error: &NSError) {
        let code = error.code();
        if code == CANCELLED || code == INTERRUPTED_BY_POLICY {
            return;
        }
        let message = error.localizedDescription().to_string();
        eprintln!("zmail: web renderer: the message did not load: {message} ({code})");
        (self.ivars().on_event)(WebEvent::LoadFailed(message));
    }
}

/// A request's URL as a string.
fn request_url(request: &NSURLRequest) -> String {
    request
        .URL()
        .and_then(|url| url.absoluteString())
        .map(|url| url.to_string())
        .unwrap_or_default()
}

/// Answers a scheme task with `body`.
fn respond(
    task: &ProtocolObject<dyn WKURLSchemeTask>,
    request: &NSURLRequest,
    mime_type: &str,
    body: &[u8],
    policy: Option<&str>,
) {
    let Some(url) = request.URL() else {
        fail(task);
        return;
    };
    let mut names = vec![
        NSString::from_str("Content-Type"),
        NSString::from_str("Content-Length"),
        NSString::from_str("Cache-Control"),
        NSString::from_str("X-Content-Type-Options"),
        NSString::from_str("Referrer-Policy"),
    ];
    let mut values = vec![
        NSString::from_str(mime_type),
        NSString::from_str(&body.len().to_string()),
        NSString::from_str("no-store"),
        NSString::from_str("nosniff"),
        NSString::from_str("no-referrer"),
    ];
    if let Some(policy) = policy {
        names.push(NSString::from_str("Content-Security-Policy"));
        values.push(NSString::from_str(policy));
    }
    let names: Vec<&NSString> = names.iter().map(|name| &**name).collect();
    let values: Vec<&NSString> = values.iter().map(|value| &**value).collect();
    let headers = NSDictionary::from_slices(&names, &values);
    let response = NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
        NSHTTPURLResponse::alloc(),
        &url,
        200,
        Some(&NSString::from_str("HTTP/1.1")),
        Some(&headers),
    );
    let Some(response) = response else {
        fail(task);
        return;
    };
    // SAFETY: the task is live and unanswered: either we are inside
    // `webView:startURLSchemeTask:`, or it is a retained remote-image task
    // that `webView:stopURLSchemeTask:` has not removed. Each call is made
    // once, in the order WebKit requires: response, data, finish.
    unsafe {
        task.didReceiveResponse(&response);
        task.didReceiveData(&NSData::with_bytes(body));
        task.didFinish();
    }
}

/// Fails a scheme task: Zmail serves nothing but its page and inline parts.
fn fail(task: &ProtocolObject<dyn WKURLSchemeTask>) {
    // SAFETY: `NSURLErrorDomain` is a valid error domain and the user info
    // may be nil; the task is live and unanswered (see `respond`).
    unsafe {
        let error = NSError::errorWithDomain_code_userInfo(NSURLErrorDomain, NOT_FOUND, None);
        task.didFailWithError(&error);
    }
}

// ---------------------------------------------------------------------------
// Content rule lists
// ---------------------------------------------------------------------------

thread_local! {
    /// Compiled rule lists by identifier.
    static RULE_LISTS: RefCell<HashMap<String, Retained<WKContentRuleList>>> =
        RefCell::new(HashMap::new());
}

/// Calls `then` with the compiled rule list, compiling it first if
/// needed. `then` runs later, on the main thread.
fn with_rule_list(
    mtm: MainThreadMarker,
    then: impl FnOnce(Result<Retained<WKContentRuleList>>) + 'static,
) {
    let json = content_rule_list();
    let identifier = rule_list_identifier(&json);
    if let Some(list) = RULE_LISTS.with(|lists| lists.borrow().get(&identifier).cloned()) {
        then(Ok(list));
        return;
    }
    // SAFETY: the default store is a WebKit singleton for this process.
    let Some(store) = (unsafe { WKContentRuleListStore::defaultStore(mtm) }) else {
        then(Err(anyhow!("WebKit has no content rule list store")));
        return;
    };
    let then = RefCell::new(Some(then));
    let key = identifier.clone();
    let started = Instant::now();
    let block = RcBlock::new(move |list: *mut WKContentRuleList, error: *mut NSError| {
        let Some(then) = then.borrow_mut().take() else {
            return;
        };
        // SAFETY: WebKit passes a compiled list or nil, and an error or nil,
        // valid for the duration of the callback; we retain the list.
        let (list, error) = unsafe { (Retained::retain(list), error.as_ref()) };
        match list {
            Some(list) => {
                debug_log(format_args!(
                    "content rule list {key} compiled in {:?}",
                    started.elapsed()
                ));
                RULE_LISTS.with(|lists| lists.borrow_mut().insert(key.clone(), list.clone()));
                then(Ok(list));
            }
            None => {
                let message = error
                    .map(|error| error.localizedDescription().to_string())
                    .unwrap_or_else(|| "unknown error".to_owned());
                eprintln!("zmail: web renderer: the content rule list did not compile: {message}");
                then(Err(anyhow!(
                    "the content blocker did not compile: {message}"
                )));
            }
        }
    });
    // SAFETY: identifier, rule list and completion handler are all non-nil;
    // WebKit calls the handler once on the main thread.
    unsafe {
        store.compileContentRuleListForIdentifier_encodedContentRuleList_completionHandler(
            Some(&NSString::from_str(&identifier)),
            Some(&NSString::from_str(&json)),
            Some(&block),
        );
    }
}

/// A web view configuration with every hardening setting, the content
/// rule list and Zmail's scheme handler.
fn hardened_configuration(
    mtm: MainThreadMarker,
    delegate: &WebDelegate,
    rule_list: &WKContentRuleList,
) -> Retained<WKWebViewConfiguration> {
    // SAFETY: plain property writes on objects created here, on the main
    // thread. `setURLSchemeHandler:forURLScheme:` raises for schemes WebKit
    // handles itself, so each scheme is checked with `handlesURLScheme:`
    // first. `setLockdownModeEnabled:` exists from macOS 13 and is only sent
    // when the object responds to it.
    unsafe {
        let configuration = WKWebViewConfiguration::new(mtm);
        configuration.setWebsiteDataStore(&WKWebsiteDataStore::nonPersistentDataStore(mtm));
        configuration.setSuppressesIncrementalRendering(true);
        configuration.setAllowsAirPlayForMediaPlayback(false);
        configuration.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::All);

        let preferences = configuration.preferences();
        preferences.setJavaScriptCanOpenWindowsAutomatically(false);
        // Safe Browsing would look URLs up with a remote service.
        preferences.setFraudulentWebsiteWarningEnabled(false);
        preferences.setElementFullscreenEnabled(false);
        preferences.setTabFocusesLinks(false);
        preferences.setTextInteractionEnabled(true);

        let page_preferences = configuration.defaultWebpagePreferences();
        page_preferences.setAllowsContentJavaScript(false);
        if page_preferences.respondsToSelector(sel!(setLockdownModeEnabled:)) {
            page_preferences.setLockdownModeEnabled(true);
        } else {
            debug_log(format_args!("Lockdown Mode is not available on this macOS"));
        }

        configuration
            .userContentController()
            .addContentRuleList(rule_list);

        let handler = ProtocolObject::from_ref(delegate);
        for scheme in [MAIL_SCHEME, CID_SCHEME] {
            let scheme = NSString::from_str(scheme);
            if WKWebView::handlesURLScheme(&scheme, mtm) {
                eprintln!("zmail: web renderer: WebKit handles the {scheme} scheme itself");
            } else {
                configuration.setURLSchemeHandler_forURLScheme(Some(handler), &scheme);
            }
        }
        configuration
    }
}

// ---------------------------------------------------------------------------
// The safe handle
// ---------------------------------------------------------------------------

/// A frame in the clip view's coordinates (points, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Frame {
    fn to_rect(self) -> NSRect {
        NSRect::new(
            NSPoint::new(self.x, self.y),
            NSSize::new(self.width.max(1.), self.height.max(1.)),
        )
    }
}

struct ViewState {
    view: RefCell<Option<Retained<MailWebView>>>,
    delegate: Retained<WebDelegate>,
    clip: Retained<ClipView>,
    frame: Cell<Option<Frame>>,
    hidden: Cell<bool>,
    /// The page has been measured once: until then it stays hidden, so no
    /// half-laid-out page flashes.
    revealed: Cell<bool>,
}

impl ViewState {
    fn apply(&self) {
        let view = self.view.borrow();
        let Some(view) = view.as_ref() else {
            return;
        };
        if let Some(frame) = self.frame.get() {
            let rect = frame.to_rect();
            if view.frame() != rect {
                view.setFrame(rect);
                view.refit(rect.size.width);
            }
        }
        view.setHidden(self.hidden.get() || !self.revealed.get());
    }
}

impl Drop for ViewState {
    fn drop(&mut self) {
        if let Some(view) = self.view.get_mut().take() {
            // SAFETY: plain calls on a live web view, detaching our
            // delegates before it goes away.
            unsafe {
                view.stopLoading();
                view.setNavigationDelegate(None);
                view.setUIDelegate(None);
            }
            view.removeFromSuperview();
        }
    }
}

/// One message's web view.
pub struct PlatformWebView {
    state: Rc<ViewState>,
}

impl PlatformWebView {
    /// Starts showing `document` in `clip`. The view appears once its
    /// content rule list is compiled and the page is measured; `on_event`
    /// hears about measurements, link clicks and failures.
    pub fn new(
        clip: &Clip,
        document: &WebDocument,
        on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<Self> {
        let mtm = main_thread()?;
        let token = next_token();
        let url = page_url(&token);
        let delegate = WebDelegate::new(
            mtm,
            DelegateIvars {
                on_event: on_event.clone(),
                page: ServedPage {
                    url: url.clone(),
                    html: page_html(document).into_bytes(),
                    policy: content_security_policy(true),
                    resources: document.resources_by_id(),
                    remote: document.remote.clone(),
                },
                view: RefCell::new(Weak::default()),
                pending: RefCell::new(Vec::new()),
                poll: RefCell::new(None),
            },
        );
        let state = Rc::new(ViewState {
            view: RefCell::new(None),
            delegate,
            clip: clip.view.clone(),
            frame: Cell::new(None),
            hidden: Cell::new(true),
            revealed: Cell::new(false),
        });
        let dark = document.draws_dark();
        let gpui_view = clip.gpui_view.clone();
        let weak_state = Rc::downgrade(&state);
        let reveal_state = Rc::downgrade(&state);
        let on_view_event: Rc<dyn Fn(WebEvent)> = Rc::new(move |event: WebEvent| {
            if matches!(event, WebEvent::Measured { .. })
                && let Some(state) = reveal_state.upgrade()
                && !state.revealed.replace(true)
            {
                state.apply();
            }
            on_event(event);
        });
        let failure = on_view_event.clone();
        with_rule_list(mtm, move |rule_list| {
            let Some(state) = RcWeak::upgrade(&weak_state) else {
                return;
            };
            let result = rule_list.and_then(|rule_list| {
                state.create(mtm, &rule_list, &url, dark, gpui_view, on_view_event)
            });
            if let Err(error) = result {
                failure(WebEvent::LoadFailed(format!("{error:#}")));
            }
        });
        Ok(Self { state })
    }

    /// Places the view in the clip view.
    pub fn set_frame(&self, frame: Frame) {
        if self.state.frame.replace(Some(frame)) != Some(frame) {
            self.state.apply();
        }
    }

    /// Hides or shows the view (it stays hidden until measured).
    pub fn set_hidden(&self, hidden: bool) {
        if self.state.hidden.replace(hidden) != hidden {
            self.state.apply();
        }
    }

    /// Copies the page's selection to the clipboard.
    pub fn copy(&self) {
        self.send_action(sel!(copy:));
    }

    /// Selects the whole page.
    pub fn select_all(&self) {
        self.send_action(sel!(selectAll:));
    }

    fn send_action(&self, action: objc2::runtime::Sel) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if let Some(view) = self.state.view.borrow().as_ref() {
            let app = NSApplication::sharedApplication(mtm);
            // SAFETY: `copy:` and `selectAll:` are standard responder
            // actions WKWebView implements, sent with no sender.
            let handled = unsafe { app.sendAction_to_from(action, Some(view), None) };
            debug_log(format_args!("{action:?} handled: {handled}"));
        }
    }

    /// Runs `script` in the isolated world and hands its string result to
    /// `then`. For the macOS tests (`tests/canary.rs`).
    pub fn evaluate(&self, script: &str, then: impl Fn(Result<String, String>) + 'static) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let view = self.state.view.borrow();
        let Some(view) = view.as_ref() else {
            then(Err("the web view does not exist yet".to_owned()));
            return;
        };
        let block = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
            // SAFETY: as in `MailWebView::measure`.
            let (result, error) = unsafe { (result.as_ref(), error.as_ref()) };
            match error {
                Some(error) => then(Err(error.localizedDescription().to_string())),
                None => then(Ok(result
                    .and_then(|result| result.downcast_ref::<NSString>())
                    .map(NSString::to_string)
                    .unwrap_or_default())),
            }
        });
        // SAFETY: as in `MailWebView::measure`.
        unsafe {
            let world = WKContentWorld::defaultClientWorld(mtm);
            view.evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                &NSString::from_str(script),
                None,
                &world,
                Some(&block),
            );
        }
    }
}

impl ViewState {
    /// Creates the web view once the rule list is ready, and loads the page.
    fn create(
        &self,
        mtm: MainThreadMarker,
        rule_list: &WKContentRuleList,
        url: &str,
        dark: bool,
        gpui_view: Weak<NSView>,
        on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<()> {
        let configuration = hardened_configuration(mtm, &self.delegate, rule_list);
        let frame = self.frame.get().map_or(
            NSRect::new(NSPoint::new(0., 0.), NSSize::new(1., 1.)),
            Frame::to_rect,
        );
        let view = MailWebView::new(
            mtm,
            frame,
            &configuration,
            MailWebViewIvars {
                gpui_view,
                on_event,
                loaded: Cell::new(false),
                fit_pending: Cell::new(true),
                fitted_width: Cell::new(frame.size.width),
            },
        );
        *self.delegate.ivars().view.borrow_mut() = Weak::from_retained(&view);
        let navigation_delegate = ProtocolObject::from_ref(&*self.delegate);
        let ui_delegate = ProtocolObject::from_ref(&*self.delegate);
        let appearance = NSAppearance::appearanceNamed(if dark {
            // SAFETY: AppKit's appearance names are immutable statics.
            unsafe { NSAppearanceNameDarkAqua }
        } else {
            // SAFETY: as above.
            unsafe { NSAppearanceNameAqua }
        });
        let url = NSURL::URLWithString(&NSString::from_str(url)).context("bad page URL")?;
        // SAFETY: plain property writes on the new web view, on the main
        // thread; the delegates are retained by `self` for the view's life.
        unsafe {
            view.setNavigationDelegate(Some(navigation_delegate));
            view.setUIDelegate(Some(ui_delegate));
            view.setAllowsBackForwardNavigationGestures(false);
            view.setAllowsMagnification(false);
            // A link preview (force click) would load the link.
            view.setAllowsLinkPreview(false);
        }
        view.unregisterDraggedTypes();
        view.setAppearance(appearance.as_deref());
        view.setHidden(true);
        self.clip.addSubview(&view);
        // SAFETY: loads Zmail's page URL; the scheme handler serves it.
        unsafe {
            view.loadRequest(&NSURLRequest::requestWithURL(&url));
        }
        debug_log(format_args!("loading {url:?} (dark: {dark})"));
        *self.view.borrow_mut() = Some(view);
        self.apply();
        Ok(())
    }
}

/// A fresh token for each page, so no two loads share a URL.
fn next_token() -> String {
    thread_local! {
        static NEXT: Cell<u64> = const { Cell::new(1) };
    }
    let count = NEXT.with(|next| next.replace(next.get() + 1));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    format!("m{count}x{nanos:x}")
}

// ---------------------------------------------------------------------------
// Test support: an offscreen window
// ---------------------------------------------------------------------------

/// An offscreen window with a clip view, for running the real web views
/// in tests without GPUI.
pub struct OffscreenWindow {
    window: Retained<NSWindow>,
    clip: Clip,
}

impl OffscreenWindow {
    /// A borderless window of `width` x `height` points, off screen.
    pub fn new(width: f64, height: f64) -> Result<Self> {
        let mtm = main_thread()?;
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        let rect = NSRect::new(NSPoint::new(-20_000., -20_000.), NSSize::new(width, height));
        // SAFETY: creates a window with valid style, backing and frame;
        // `releasedWhenClosed` is turned off because Rust owns it.
        let window = unsafe {
            let window = NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            );
            window.setReleasedWhenClosed(false);
            window
        };
        let content = window
            .contentView()
            .context("the window has no content view")?;
        let container = NSView::initWithFrame(NSView::alloc(mtm), content.bounds());
        content.addSubview(&container);
        window.orderFrontRegardless();
        let raw_container = Retained::as_ptr(&container) as usize;
        let raw_content = NonNull::from(&*content).cast::<c_void>();
        let clip = Clip::attach(raw_container, raw_content)?;
        Ok(Self { window, clip })
    }

    /// The clip view to create web views in.
    pub fn clip(&self) -> &Clip {
        &self.clip
    }

    /// Runs the main run loop for `duration`, so WebKit's callbacks arrive.
    pub fn pump(&self, duration: Duration) {
        let deadline = Instant::now() + duration;
        let run_loop = NSRunLoop::currentRunLoop();
        while Instant::now() < deadline {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.02);
            // SAFETY: the default mode is a valid run loop mode.
            unsafe {
                run_loop.runMode_beforeDate(NSDefaultRunLoopMode, &until);
            }
        }
    }
}

impl Drop for OffscreenWindow {
    fn drop(&mut self) {
        self.window.close();
    }
}
