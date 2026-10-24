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

use crate::{WebDocument, WebEvent};

/// Which web view: the reader uses the message id.
pub type WebKey = u64;

/// One reader's web views.
#[derive(Clone)]
pub struct WebHost {
    state: Rc<RefCell<HostState>>,
}

struct HostState {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    on_event: Rc<dyn Fn(WebKey, WebEvent)>,
    /// Why the web renderer cannot be used in this window, once it failed.
    unavailable: Option<String>,
    /// The list's viewport this frame, in window coordinates.
    viewport: Option<Bounds<Pixels>>,
    /// The web views a body placed this frame.
    touched: HashSet<WebKey>,
    #[cfg(target_os = "macos")]
    backend: macos::Backend,
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
                #[cfg(target_os = "macos")]
                backend: macos::Backend::default(),
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
        #[cfg(target_os = "macos")]
        self.state.borrow_mut().backend.retain(&keep);
        #[cfg(not(target_os = "macos"))]
        let _ = keep;
    }

    /// Hides every web view until a body places it again: for when the
    /// reader stops drawing its list (another pane shows instead, or no
    /// conversation is open).
    pub fn hide_all(&self) {
        #[cfg(target_os = "macos")]
        self.state.borrow_mut().backend.hide_all();
    }

    /// Copies the selection in message `key`'s page.
    pub fn copy(&self, key: WebKey) {
        #[cfg(target_os = "macos")]
        self.state.borrow().backend.copy(key);
        #[cfg(not(target_os = "macos"))]
        let _ = key;
    }

    /// Selects all of message `key`'s page.
    pub fn select_all(&self, key: WebKey) {
        #[cfg(target_os = "macos")]
        self.state.borrow().backend.select_all(key);
        #[cfg(not(target_os = "macos"))]
        let _ = key;
    }

    fn begin_frame(&self, viewport: Bounds<Pixels>) {
        let mut state = self.state.borrow_mut();
        state.viewport = Some(viewport);
        state.touched.clear();
    }

    fn end_frame(&self, window: &mut Window) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        #[cfg(target_os = "macos")]
        state.backend.end_frame(&state.touched, window);
        #[cfg(not(target_os = "macos"))]
        let _ = window;
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
        #[cfg(target_os = "macos")]
        {
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
        #[cfg(not(target_os = "macos"))]
        let _ = (document, bounds, viewport, window);
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

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        collections::{HashMap, HashSet},
        rc::Rc,
    };

    use anyhow::{Context as _, Result, bail};
    use gpui::{Bounds, Pixels, Window, WindowCompositionSurface};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::WebKey;
    use crate::platform::macos::{Clip, Frame, PlatformWebView};
    use crate::{WebDocument, WebEvent, debug_log};

    /// The composition surface and the web views in it.
    #[derive(Default)]
    pub(super) struct Backend {
        surface: Option<Surface>,
        views: HashMap<WebKey, Slot>,
    }

    struct Surface {
        // Field order: the clip view goes before the surface it is in.
        clip: Clip,
        surface: WindowCompositionSurface,
        bounds: Option<Bounds<Pixels>>,
        visible: bool,
    }

    struct Slot {
        view: PlatformWebView,
        document: Rc<WebDocument>,
    }

    impl Backend {
        pub(super) fn place(
            &mut self,
            key: WebKey,
            document: &Rc<WebDocument>,
            bounds: Bounds<Pixels>,
            viewport: Bounds<Pixels>,
            window: &mut Window,
            on_event: Rc<dyn Fn(WebEvent)>,
        ) -> Result<()> {
            self.ensure_surface(viewport, window)?;
            let surface = self.surface.as_ref().context("no surface")?;
            let replace = self.views.get(&key).is_none_or(|slot| {
                !Rc::ptr_eq(&slot.document, document) && *slot.document != **document
            });
            if replace {
                debug_log(format_args!("creating the web view for message {key}"));
                let view = PlatformWebView::new(&surface.clip, document, on_event)?;
                self.views.insert(
                    key,
                    Slot {
                        view,
                        document: document.clone(),
                    },
                );
            }
            if let Some(slot) = self.views.get(&key) {
                let x = f64::from(f32::from(bounds.origin.x - viewport.origin.x));
                let y = f64::from(f32::from(bounds.origin.y - viewport.origin.y));
                slot.view.set_frame(Frame {
                    x,
                    y,
                    width: f64::from(f32::from(bounds.size.width)),
                    height: f64::from(f32::from(bounds.size.height)),
                });
                slot.view.set_hidden(!bounds.intersects(&viewport));
            }
            Ok(())
        }

        /// Creates the surface on first use, and keeps it on `viewport`.
        fn ensure_surface(&mut self, viewport: Bounds<Pixels>, window: &mut Window) -> Result<()> {
            if self.surface.is_none() {
                let composition = window
                    .enable_window_composition()
                    .context("enabling window composition")?;
                let surface = composition
                    .create_native_surface()
                    .context("creating the native surface")?;
                let handle = surface
                    .platform_surface()?
                    .platform_handle()?
                    .downcast::<usize>()
                    .map_err(|_| anyhow::anyhow!("the native surface is not an AppKit view"))?;
                let gpui_view = match HasWindowHandle::window_handle(window)
                    .map_err(|error| anyhow::anyhow!("no window handle: {error:?}"))?
                    .as_raw()
                {
                    RawWindowHandle::AppKit(handle) => handle.ns_view,
                    _ => bail!("the window is not an AppKit window"),
                };
                let clip = Clip::attach(*handle, gpui_view)?;
                debug_log(format_args!("native composition surface created"));
                self.surface = Some(Surface {
                    clip,
                    surface,
                    bounds: None,
                    visible: true,
                });
            }
            let surface = self.surface.as_mut().context("no surface")?;
            if surface.bounds != Some(viewport) {
                surface
                    .surface
                    .platform_surface()?
                    .set_bounds(viewport.to_device_pixels(window.scale_factor()))?;
                surface.bounds = Some(viewport);
            }
            Ok(())
        }

        pub(super) fn end_frame(&mut self, touched: &HashSet<WebKey>, _window: &mut Window) {
            for (key, slot) in &self.views {
                if !touched.contains(key) {
                    slot.view.set_hidden(true);
                }
            }
            self.set_visible(!touched.is_empty());
        }

        fn set_visible(&mut self, visible: bool) {
            if let Some(surface) = self.surface.as_mut()
                && surface.visible != visible
            {
                surface.visible = visible;
                surface.clip.set_visible(visible);
                if let Ok(platform) = surface.surface.platform_surface()
                    && let Err(error) = platform.set_visible(visible)
                {
                    eprintln!("zmail: web renderer: could not show or hide the surface: {error:#}");
                }
            }
        }

        pub(super) fn retain(&mut self, keep: &dyn Fn(WebKey) -> bool) {
            self.views.retain(|key, _| keep(*key));
        }

        pub(super) fn hide_all(&mut self) {
            for slot in self.views.values() {
                slot.view.set_hidden(true);
            }
            self.set_visible(false);
        }

        pub(super) fn copy(&self, key: WebKey) {
            if let Some(slot) = self.views.get(&key) {
                slot.view.copy();
            }
        }

        pub(super) fn select_all(&self, key: WebKey) {
            if let Some(slot) = self.views.get(&key) {
                slot.view.select_all();
            }
        }
    }
}
