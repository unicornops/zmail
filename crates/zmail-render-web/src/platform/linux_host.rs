//! The Linux backend of [`crate::host::WebHost`]. Safe code only; the
//! WebKitGTK calls are in `platform::linux`, on the engine thread.
//!
//! **X11**: each message's web view is a GTK window embedded in a native
//! composition surface of its own: a child window GPUI creates in its
//! window, kept on the part of the body the conversation list shows. The
//! page is live, with WebKit's own selection, link cursor and hover; the
//! surface clips it to the list, and GPUI cuts its menus and dialogs out
//! of the surface so they draw above it. (One surface over the whole list,
//! as on macOS, would hide what GPUI draws around the pages: an X11 child
//! window is opaque.) Vertical wheel turns over a page come back from the
//! engine and are dispatched to GPUI's window, so the conversation scrolls
//! as one list.
//!
//! **Wayland**: GTK cannot put anything inside a surface of GPUI's own
//! Wayland connection, so each web view renders offscreen and the body
//! paints pictures of its page (ADR 0006): GPUI clips them, scrolls them
//! and draws its overlays above them like any image. GPUI hands the body
//! its mouse input: a click on a link (found in the picture's link map)
//! opens it through Zmail, and a drag selects text in the page, through
//! Zmail's isolated script world, and draws the page again. Copy reads the
//! selection the same way and puts it on GPUI's clipboard.
//!
//! The engine reports on a channel per view; a GPUI task per view reads it.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Result, anyhow, bail};
use futures_channel::mpsc::unbounded;
use gpui::{
    App, AppContext as _, AsyncApp, Bounds, ClipboardItem, Corners, CursorStyle, DispatchPhase,
    Hitbox, Image, ImageFormat, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, PlatformInput, Point, RenderImage, ScrollDelta, ScrollWheelEvent, Task,
    TouchPhase, Window, WindowCompositionSurface, point, px, size,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::host::WebKey;
use crate::platform::linux::{self, DisplayServer, PlatformWebView, Snapshot, ViewEvent, ViewSize};
use crate::script::{SELECT_ALL_SCRIPT, SELECTION_SCRIPT};
use crate::{WebDocument, WebEvent, debug_log};

/// Lines GPUI scrolls per wheel notch on Linux (`gpui_linux`'s
/// `SCROLL_LINES`), so a wheel turn over a page scrolls as it does
/// anywhere else in the window.
const SCROLL_LINES: f32 = 3.0;

/// The tag of the selection read for Copy (Wayland).
const COPY_TAG: u64 = 1;

/// How far the mouse may move between press and release for a click.
const CLICK_SLOP: f32 = 4.;

/// The web views, their surfaces (X11) and pictures (Wayland).
#[derive(Default)]
pub(crate) struct Backend {
    display: Option<DisplayServer>,
    views: HashMap<WebKey, Slot>,
    /// Pictures no longer shown, to drop from GPUI's sprite atlas at the
    /// next paint (Wayland).
    retired: Rc<RefCell<Vec<Arc<RenderImage>>>>,
}

struct Slot {
    view: Rc<PlatformWebView>,
    /// The view's surface on X11; destroyed after the view (see
    /// [`Backend::release`]).
    surface: Option<Surface>,
    document: Rc<WebDocument>,
    on_event: Rc<dyn Fn(WebEvent)>,
    size: Option<ViewSize>,
    position: Option<(i32, i32)>,
    /// The view's top-left corner in window coordinates, for wheel turns.
    origin: Rc<Cell<Point<Pixels>>>,
    /// The page has been measured: until then an X11 surface stays hidden,
    /// so no empty (black) window shows while it loads.
    measured: Rc<Cell<bool>>,
    /// The latest picture of the page (Wayland).
    picture: Rc<RefCell<Option<Picture>>>,
    /// Where a press in the page started, in body coordinates (Wayland).
    press: Rc<Cell<Option<Point<Pixels>>>>,
    /// Reads the view's events; dropped with the slot.
    _events: Task<()>,
}

/// A view's native surface on X11: the part of the body the list shows.
struct Surface {
    surface: WindowCompositionSurface,
    /// The surface's X11 window, which the view is embedded in.
    window: u64,
    bounds: Option<Bounds<Pixels>>,
    visible: bool,
}

impl Surface {
    fn create(window: &mut Window) -> Result<Self> {
        let composition = window
            .enable_window_composition()
            .map_err(|error| anyhow!("enabling window composition: {error:#}"))?;
        let surface = composition
            .create_native_surface()
            .map_err(|error| anyhow!("creating a native surface: {error:#}"))?;
        let handle = surface
            .platform_surface()?
            .platform_handle()?
            .downcast::<RawWindowHandle>()
            .map_err(|_| anyhow!("the native surface has no window handle"))?;
        let RawWindowHandle::Xcb(handle) = *handle else {
            bail!("the native surface is not an X11 window");
        };
        debug_log(format_args!(
            "native composition surface created (X11 window {:#x})",
            handle.window
        ));
        Ok(Self {
            surface,
            window: u64::from(handle.window.get()),
            bounds: None,
            visible: true,
        })
    }

    /// Puts the surface on `bounds` (window coordinates), or hides it.
    fn show(&mut self, bounds: Option<Bounds<Pixels>>, scale: f32) {
        let result = (|| -> Result<()> {
            let platform = self.surface.platform_surface()?;
            if let Some(bounds) = bounds
                && self.bounds != Some(bounds)
            {
                platform.set_bounds(bounds.to_device_pixels(scale))?;
                self.bounds = Some(bounds);
            }
            let visible = bounds.is_some();
            if self.visible != visible {
                platform.set_visible(visible)?;
                self.visible = visible;
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("zmail: web renderer: could not place a surface: {error:#}");
        }
    }
}

/// A page's picture, in the body's logical pixels (Wayland).
struct Picture {
    tiles: Vec<(Bounds<Pixels>, Arc<RenderImage>)>,
    links: Vec<(Bounds<Pixels>, String)>,
    /// CSS pixels per logical pixel, for selection points.
    css_per_point: f64,
}

impl Picture {
    fn link_at(&self, at: Point<Pixels>) -> Option<&str> {
        self.links
            .iter()
            .find(|(bounds, _)| bounds.contains(&at))
            .map(|(_, url)| url.as_str())
    }
}

impl Backend {
    /// Pictures take no input of their own (Wayland), so the body hands
    /// them GPUI's. On X11 the GTK window above the hitbox gets it first.
    pub(crate) const FORWARDS_INPUT: bool = true;

    // The host's arguments, one per thing a backend needs; see `WebHost::place`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place(
        &mut self,
        key: WebKey,
        document: &Rc<WebDocument>,
        bounds: Bounds<Pixels>,
        viewport: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
        on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<()> {
        let display = self.display(window)?;
        linux::start(display).map_err(|reason| anyhow!(reason))?;
        stop_engine_on_quit(cx);
        let scale = window.scale_factor();
        let replace = self.views.get(&key).is_none_or(|slot| {
            !Rc::ptr_eq(&slot.document, document) && *slot.document != **document
        });
        if replace {
            debug_log(format_args!("creating the web view for message {key}"));
            let surface = match display {
                DisplayServer::X11 => Some(Surface::create(window)?),
                DisplayServer::Wayland => None,
            };
            let size = view_size(bounds, scale);
            let (sender, mut receiver) = unbounded();
            let parent = surface.as_ref().map(|surface| surface.window);
            let view = Rc::new(
                PlatformWebView::new(document, size, parent, sender)
                    .map_err(|reason| anyhow!(reason))?,
            );
            let origin = Rc::new(Cell::new(bounds.origin));
            let measured = Rc::new(Cell::new(false));
            let picture = Rc::new(RefCell::new(None));
            let handle = Window::window_handle(window);
            let (task_origin, task_measured, task_picture, task_on_event, retired) = (
                origin.clone(),
                measured.clone(),
                picture.clone(),
                on_event.clone(),
                self.retired.clone(),
            );
            let events = cx.spawn(async move |cx: &mut AsyncApp| {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        ViewEvent::Web(event) => {
                            if matches!(event, WebEvent::Measured { .. }) {
                                task_measured.set(true);
                            }
                            task_on_event(event);
                        }
                        ViewEvent::Scroll {
                            x,
                            y,
                            lines,
                            control,
                        } => {
                            let event = ScrollWheelEvent {
                                position: task_origin.get() + point(px(x as f32), px(y as f32)),
                                // GPUI's lines are positive upwards.
                                delta: ScrollDelta::Lines(point(
                                    0.,
                                    -(lines as f32) * SCROLL_LINES,
                                )),
                                modifiers: Modifiers {
                                    control,
                                    ..Modifiers::default()
                                },
                                touch_phase: TouchPhase::Moved,
                            };
                            let _ = cx.update_window(handle, |_, window, cx| {
                                window.dispatch_event(PlatformInput::ScrollWheel(event), cx);
                            });
                        }
                        ViewEvent::Snapshot(snapshot) => {
                            let new = cx.update(|cx| decode(snapshot, cx));
                            if let Some(old) = task_picture.replace(Some(new)) {
                                retired
                                    .borrow_mut()
                                    .extend(old.tiles.into_iter().map(|(_, image)| image));
                            }
                            let _ = cx.update_window(handle, |_, window, _| window.refresh());
                        }
                        ViewEvent::Script {
                            tag: COPY_TAG,
                            result: Ok(text),
                        } if !text.is_empty() => {
                            cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text)));
                        }
                        ViewEvent::Script { .. } => {}
                    }
                }
            });
            let replaced = self.views.insert(
                key,
                Slot {
                    view,
                    surface,
                    document: document.clone(),
                    on_event,
                    size: Some(size),
                    position: None,
                    origin,
                    measured,
                    picture,
                    press: Rc::new(Cell::new(None)),
                    _events: events,
                },
            );
            if let Some(old) = replaced {
                self.release(vec![old]);
            }
        }
        if let Some(slot) = self.views.get_mut(&key) {
            slot.origin.set(bounds.origin);
            let size = view_size(bounds, scale);
            if slot.size != Some(size) {
                slot.size = Some(size);
                slot.view.resize(size);
            }
            if let Some(surface) = slot.surface.as_mut() {
                // The surface covers what the list shows of the body; the
                // page sits in it where the body's top-left corner is.
                let shown = bounds.intersect(&viewport);
                if slot.measured.get() && shown.size.width > px(0.) && shown.size.height > px(0.) {
                    surface.show(Some(shown), scale);
                    let position = (
                        device(bounds.origin.x - shown.origin.x, scale),
                        device(bounds.origin.y - shown.origin.y, scale),
                    );
                    if slot.position != Some(position) {
                        slot.position = Some(position);
                        slot.view.move_to(position.0, position.1, true);
                    }
                } else {
                    surface.show(None, scale);
                }
            }
        }
        Ok(())
    }

    /// The display server GPUI runs on, from the window's handle.
    fn display(&mut self, window: &Window) -> Result<DisplayServer> {
        if let Some(display) = self.display {
            return Ok(display);
        }
        let display = match HasWindowHandle::window_handle(window)
            .map_err(|error| anyhow!("no window handle: {error:?}"))?
            .as_raw()
        {
            RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => DisplayServer::X11,
            RawWindowHandle::Wayland(_) => DisplayServer::Wayland,
            other => bail!("unexpected window handle {other:?}"),
        };
        self.display = Some(display);
        Ok(display)
    }

    /// Paints a Wayland page's picture and takes the body's mouse input.
    pub(crate) fn paint(
        &mut self,
        key: WebKey,
        bounds: Bounds<Pixels>,
        hitbox: &Hitbox,
        window: &mut Window,
    ) {
        for image in self.retired.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
        if self.display != Some(DisplayServer::Wayland) {
            return;
        }
        let Some(slot) = self.views.get(&key) else {
            return;
        };
        if let Some(picture) = slot.picture.borrow().as_ref() {
            for (tile, image) in &picture.tiles {
                let tile = Bounds::new(bounds.origin + tile.origin, tile.size);
                let _ = window.paint_image(tile, tile, Corners::default(), image.clone(), 0, false);
            }
            let over_link = picture
                .link_at(window.mouse_position() - bounds.origin)
                .is_some();
            window.set_cursor_style(
                if over_link {
                    CursorStyle::PointingHand
                } else {
                    CursorStyle::IBeam
                },
                hitbox,
            );
        }

        let (view, picture, press, on_event, hitbox_down) = (
            slot.view.clone(),
            slot.picture.clone(),
            slot.press.clone(),
            slot.on_event.clone(),
            hitbox.clone(),
        );
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox_down.is_hovered(window)
            {
                return;
            }
            let at = event.position - bounds.origin;
            press.set(Some(at));
            on_event(WebEvent::Pressed);
            // A press starts a new selection (and clears the old one).
            if let Some(picture) = picture.borrow().as_ref() {
                view.evaluate(&select_script(picture, at, at), 0);
                view.request_snapshot();
            }
            cx.stop_propagation();
        });

        let (view, picture, press) = (slot.view.clone(), slot.picture.clone(), slot.press.clone());
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, _cx| {
            if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            let Some(start) = press.get() else {
                return;
            };
            if let Some(picture) = picture.borrow().as_ref() {
                view.evaluate(
                    &select_script(picture, start, event.position - bounds.origin),
                    0,
                );
                view.request_snapshot();
            }
        });

        let (picture, press, on_event) = (
            slot.picture.clone(),
            slot.press.clone(),
            slot.on_event.clone(),
        );
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            let Some(start) = press.take() else {
                return;
            };
            let at = event.position - bounds.origin;
            let moved = at - start;
            let click =
                f32::from(moved.x).abs() <= CLICK_SLOP && f32::from(moved.y).abs() <= CLICK_SLOP;
            let link = picture
                .borrow()
                .as_ref()
                .and_then(|picture| picture.link_at(at).map(str::to_owned));
            if click && let Some(url) = link {
                on_event(WebEvent::LinkActivated { url });
            }
            cx.stop_propagation();
        });
    }

    pub(crate) fn end_frame(&mut self, touched: &HashSet<WebKey>, window: &mut Window) {
        let scale = window.scale_factor();
        for (key, slot) in &mut self.views {
            if !touched.contains(key)
                && let Some(surface) = slot.surface.as_mut()
            {
                surface.show(None, scale);
            }
        }
    }

    /// Drops `slots`. GPUI destroys a surface's window (and the GTK window
    /// in it) when the surface is dropped, so the engine destroys the
    /// views first; pictures leave GPUI's atlas at the next paint.
    fn release(&self, slots: Vec<Slot>) {
        let mut surfaces = Vec::with_capacity(slots.len());
        let mut views = Vec::with_capacity(slots.len());
        for slot in slots {
            if let Some(picture) = slot.picture.borrow_mut().take() {
                self.retired
                    .borrow_mut()
                    .extend(picture.tiles.into_iter().map(|(_, image)| image));
            }
            surfaces.push(slot.surface);
            // The paint handlers hold the view too; they go with the frame.
            if let Some(view) = Rc::into_inner(slot.view) {
                views.push(view);
            }
        }
        linux::destroy_now(views);
        drop(surfaces);
    }

    pub(crate) fn retain(&mut self, keep: &dyn Fn(WebKey) -> bool) {
        let dropped: Vec<WebKey> = self
            .views
            .keys()
            .copied()
            .filter(|key| !keep(*key))
            .collect();
        let slots = dropped
            .iter()
            .filter_map(|key| self.views.remove(key))
            .collect();
        self.release(slots);
    }

    pub(crate) fn hide_all(&mut self) {
        for slot in self.views.values_mut() {
            if let Some(surface) = slot.surface.as_mut() {
                // The scale only matters for a surface being shown.
                surface.show(None, 1.);
            }
        }
    }

    pub(crate) fn copy(&self, key: WebKey) {
        let Some(slot) = self.views.get(&key) else {
            return;
        };
        match self.display {
            // GTK's own clipboard (an X11 selection) works for every app.
            Some(DisplayServer::X11) => slot.view.copy(),
            // GTK has no focused surface on Wayland to own the clipboard
            // with, so GPUI's clipboard takes the selection's text.
            _ => slot.view.evaluate(SELECTION_SCRIPT, COPY_TAG),
        }
    }

    pub(crate) fn select_all(&self, key: WebKey) {
        let Some(slot) = self.views.get(&key) else {
            return;
        };
        match self.display {
            Some(DisplayServer::X11) => slot.view.select_all(),
            _ => {
                slot.view.evaluate(SELECT_ALL_SCRIPT, 0);
                slot.view.request_snapshot();
            }
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        let slots = self.views.drain().map(|(_, slot)| slot).collect();
        self.release(slots);
    }
}

/// Stops the engine when the app quits (once per process), so WebKit is
/// idle on its thread before the process exits.
fn stop_engine_on_quit(cx: &mut App) {
    static HOOKED: AtomicBool = AtomicBool::new(false);
    if !HOOKED.swap(true, Ordering::SeqCst) {
        cx.on_app_quit(|_| async { linux::shutdown() }).detach();
    }
}

/// Turns a snapshot's BMP tiles into GPUI images.
fn decode(snapshot: Snapshot, cx: &mut App) -> Picture {
    let scale = snapshot.scale.max(0.5);
    let to_points = |device: u32| px((f64::from(device) / scale) as f32);
    let tiles = snapshot
        .tiles
        .into_iter()
        .filter_map(|tile| {
            let bounds = Bounds::new(
                point(px(0.), to_points(tile.top)),
                size(to_points(tile.width), to_points(tile.height)),
            );
            match Image::from_bytes(ImageFormat::Bmp, tile.bmp).to_image_data(cx.svg_renderer()) {
                Ok(image) => Some((bounds, image)),
                Err(error) => {
                    eprintln!(
                        "zmail: web renderer: a picture of the page did not decode: {error:#}"
                    );
                    None
                }
            }
        })
        .collect();
    let links = snapshot
        .links
        .into_iter()
        .map(|link| {
            (
                Bounds::new(
                    point(px(link.x as f32), px(link.y as f32)),
                    size(px(link.width as f32), px(link.height as f32)),
                ),
                link.url,
            )
        })
        .collect();
    Picture {
        tiles,
        links,
        css_per_point: 1. / snapshot.zoom.max(0.01),
    }
}

/// The isolated-world script selecting the page's text from `from` to
/// `to` (body coordinates). The offscreen view is only about a screen
/// tall, so it scrolls to each point before finding the text there.
fn select_script(picture: &Picture, from: Point<Pixels>, to: Point<Pixels>) -> String {
    let css = |value: Pixels| f64::from(f32::from(value)) * picture.css_per_point;
    format!(
        "(() => {{ const at = (x, y) => {{ scrollTo(0, Math.max(0, y - innerHeight / 2)); \
         return document.caretRangeFromPoint(x - scrollX, y - scrollY); }}; \
         const a = at({}, {}), b = at({}, {}), s = getSelection(); \
         if (!a || !b || (a.startContainer === b.startContainer && a.startOffset === b.startOffset)) \
         {{ s.removeAllRanges(); return ''; }} \
         s.setBaseAndExtent(a.startContainer, a.startOffset, b.startContainer, b.startOffset); \
         return ''; }})()",
        css(from.x),
        css(from.y),
        css(to.x),
        css(to.y)
    )
}

fn view_size(bounds: Bounds<Pixels>, scale: f32) -> ViewSize {
    ViewSize {
        width: f64::from(f32::from(bounds.size.width)),
        height: f64::from(f32::from(bounds.size.height)),
        scale: f64::from(scale),
    }
}

/// Logical pixels to device pixels, clamped to X11's 16-bit coordinates.
fn device(value: Pixels, scale: f32) -> i32 {
    (f64::from(f32::from(value)) * f64::from(scale))
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i32
}
