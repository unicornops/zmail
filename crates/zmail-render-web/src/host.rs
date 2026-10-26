//! The GPUI side: one [`WebHost`] per reader.
//!
//! The host owns one native composition surface for the reader, covering
//! the conversation list's viewport (the [`WebViewport`] element wraps the
//! list and records its bounds each frame), and a web view per message
//! shown formatted. A [`WebBody`] element stands in for a message's body in
//! the list: it takes the page's measured height, and in prepaint moves its
//! web view to where the element is, relative to the viewport.
//!
//! Moving web views inside the surface is plain AppKit work; the surface
//! itself only changes bounds when the viewport does (a resize), because
//! every surface geometry change makes GPUI re-sync the whole composition
//! tree. The surface's view clips to its bounds, so a message scrolled
//! partly out of the list is cut off at the list's edges and never covers
//! the action bar above it.
//!
//! After the list has prepainted, the viewport hides every web view no
//! body touched this frame (scrolled out of the list, collapsed, switched
//! to the simple view), and the whole surface when none is left.

use std::{cell::RefCell, collections::HashSet, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, Pixels, Style, Window, px, relative,
};

use crate::{WebDocument, WebEvent, platform::Backend};

/// Which web view: the reader uses the message id.
pub type WebKey = u64;

/// One reader's web views.
#[derive(Clone)]
pub struct WebHost {
    state: Rc<RefCell<HostState>>,
}

struct HostState {
    on_event: Rc<dyn Fn(WebKey, WebEvent)>,
    /// Why the web renderer cannot be used in this window, once it failed.
    unavailable: Option<String>,
    /// The list's viewport this frame, in window coordinates.
    viewport: Option<Bounds<Pixels>>,
    /// The web views a body placed this frame.
    touched: HashSet<WebKey>,
    backend: Backend,
}

impl WebHost {
    /// A host whose web views report to `on_event`. The callback runs
    /// outside GPUI's update cycle (from AppKit), so it should only
    /// schedule work, e.g. with `AsyncApp::spawn`.
    pub fn new(on_event: impl Fn(WebKey, WebEvent) + 'static) -> Self {
        Self {
            state: Rc::new(RefCell::new(HostState {
                on_event: Rc::new(on_event),
                unavailable: (!crate::SUPPORTED)
                    .then(|| "this platform has no web renderer".to_owned()),
                viewport: None,
                touched: HashSet::new(),
                backend: Backend::default(),
            })),
        }
    }

    /// Whether messages can be shown formatted in this window.
    pub fn is_available(&self) -> bool {
        self.state.borrow().unavailable.is_none()
    }

    /// Why messages cannot be shown formatted, if they cannot.
    pub fn unavailable_reason(&self) -> Option<String> {
        self.state.borrow().unavailable.clone()
    }

    /// Wraps the conversation list, so the host knows its viewport and can
    /// hide what the list did not place this frame.
    pub fn viewport(&self, list: impl IntoElement) -> WebViewport {
        WebViewport {
            host: self.clone(),
            child: list.into_any_element(),
        }
    }

    /// The element standing in for message `key`'s body: `height` tall
    /// (the last measured height), showing `document`.
    pub fn body(&self, key: WebKey, document: Rc<WebDocument>, height: Pixels) -> WebBody {
        WebBody {
            host: self.clone(),
            key,
            document,
            height,
        }
    }

    /// Drops the web views of the messages `keep` returns false for.
    pub fn retain(&self, keep: impl Fn(WebKey) -> bool) {
        self.state.borrow_mut().backend.retain(&keep);
    }

    /// Hides every web view until a body places it again: for when the
    /// reader stops drawing its list (another pane shows instead, or no
    /// conversation is open).
    pub fn hide_all(&self) {
        self.state.borrow_mut().backend.hide_all();
    }

    /// Copies the selection in message `key`'s page.
    pub fn copy(&self, key: WebKey) {
        self.state.borrow().backend.copy(key);
    }

    /// Selects all of message `key`'s page.
    pub fn select_all(&self, key: WebKey) {
        self.state.borrow().backend.select_all(key);
    }

    fn begin_frame(&self, viewport: Bounds<Pixels>) {
        let mut state = self.state.borrow_mut();
        state.viewport = Some(viewport);
        state.touched.clear();
    }

    fn end_frame(&self, window: &mut Window) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        state.backend.end_frame(&state.touched, window);
        state.viewport = None;
    }

    fn place(
        &self,
        key: WebKey,
        document: &Rc<WebDocument>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        if state.unavailable.is_some() {
            return;
        }
        let Some(viewport) = state.viewport else {
            return;
        };
        state.touched.insert(key);
        let on_event = state.on_event.clone();
        let result = state.backend.place(
            key,
            document,
            bounds,
            viewport,
            window,
            Rc::new(move |event| on_event(key, event)),
        );
        if let Err(error) = result {
            let reason = format!("{error:#}");
            eprintln!("zmail: web renderer: unavailable in this window: {reason}");
            state.unavailable = Some(reason.clone());
            let on_event = state.on_event.clone();
            // Reported after this frame, outside the host's borrow.
            window.on_next_frame(move |_, _| on_event(key, WebEvent::LoadFailed(reason)));
        }
    }
}

/// Wraps the conversation list; see [`WebHost::viewport`].
pub struct WebViewport {
    host: WebHost,
    child: AnyElement,
}

impl IntoElement for WebViewport {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebViewport {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The wrapper is the list: it takes the list's layout node.
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.host.begin_frame(bounds);
        self.child.prepaint(window, cx);
        self.host.end_frame(window);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// A message body shown by a web view; see [`WebHost::body`].
pub struct WebBody {
    host: WebHost,
    key: WebKey,
    document: Rc<WebDocument>,
    height: Pixels,
}

impl IntoElement for WebBody {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for WebBody {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = self.height.max(px(1.)).into();
        style.min_size.width = px(0.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        self.host.place(self.key, &self.document, bounds, window);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }
}
