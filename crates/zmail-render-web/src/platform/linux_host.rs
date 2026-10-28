//! The Linux backend of [`crate::host::WebHost`]. Safe code only; the
//! WebKitGTK calls are in `platform::linux`, on the engine thread.
//!
//! On X11 each message's web view is a GTK window embedded in a native
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
//! The engine reports on a channel per view; a GPUI task per view reads it
//! and calls the host's callback.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::{Result, anyhow, bail};
use futures_channel::mpsc::unbounded;
use gpui::{
    App, AppContext as _, Bounds, Hitbox, Modifiers, Pixels, PlatformInput, Point, ScrollDelta,
    ScrollWheelEvent, Task, TouchPhase, Window, WindowCompositionSurface, point, px,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::host::WebKey;
use crate::platform::linux::{self, DisplayServer, PlatformWebView, ViewEvent, ViewSize};
use crate::{WebDocument, WebEvent, debug_log};

/// Lines GPUI scrolls per wheel notch on Linux (`gpui_linux`'s
/// `SCROLL_LINES`), so a wheel turn over a page scrolls as it does
/// anywhere else in the window.
const SCROLL_LINES: f32 = 3.0;

/// The web views and their surfaces.
#[derive(Default)]
pub(crate) struct Backend {
    display: Option<DisplayServer>,
    views: HashMap<WebKey, Slot>,
}

struct Slot {
    view: PlatformWebView,
    /// Destroyed after the view (see [`release`]).
    surface: Surface,
    document: Rc<WebDocument>,
    size: Option<ViewSize>,
    position: Option<(i32, i32)>,
    /// The view's top-left corner in window coordinates, for wheel turns.
    origin: Rc<Cell<Point<Pixels>>>,
    /// The page has been measured: until then its surface stays hidden,
    /// so no empty (black) window shows while it loads.
    measured: Rc<Cell<bool>>,
    /// Reads the view's events; dropped with the slot.
    _events: Task<()>,
}

/// A view's native surface: the part of the body the list shows.
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

impl Backend {
    /// The GTK window takes its own mouse input from X11.
    pub(crate) const FORWARDS_INPUT: bool = false;

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
        if display == DisplayServer::Wayland {
            bail!("formatted mail is not available on Wayland yet");
        }
        let scale = window.scale_factor();
        let replace = self.views.get(&key).is_none_or(|slot| {
            !Rc::ptr_eq(&slot.document, document) && *slot.document != **document
        });
        if replace {
            debug_log(format_args!("creating the web view for message {key}"));
            let surface = Surface::create(window)?;
            let size = view_size(bounds, scale);
            let (sender, mut receiver) = unbounded();
            let view = PlatformWebView::new(document, size, Some(surface.window), sender)
                .map_err(|reason| anyhow!(reason))?;
            let origin = Rc::new(Cell::new(bounds.origin));
            let wheel_origin = origin.clone();
            let measured = Rc::new(Cell::new(false));
            let page_measured = measured.clone();
            let handle = Window::window_handle(window);
            let events = cx.spawn(async move |cx| {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        ViewEvent::Web(event) => {
                            if matches!(event, WebEvent::Measured { .. }) {
                                page_measured.set(true);
                            }
                            on_event(event);
                        }
                        ViewEvent::Scroll {
                            x,
                            y,
                            lines,
                            control,
                        } => {
                            let position = wheel_origin.get() + point(px(x as f32), px(y as f32));
                            let event = ScrollWheelEvent {
                                position,
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
                    size: Some(size),
                    position: None,
                    origin,
                    measured,
                    _events: events,
                },
            );
            if let Some(old) = replaced {
                release(vec![old]);
            }
        }
        if let Some(slot) = self.views.get_mut(&key) {
            slot.origin.set(bounds.origin);
            let size = view_size(bounds, scale);
            if slot.size != Some(size) {
                slot.size = Some(size);
                slot.view.resize(size);
            }
            // The surface covers what the list shows of the body; the page
            // sits in it where the body's top-left corner is.
            let shown = bounds.intersect(&viewport);
            if slot.measured.get() && shown.size.width > px(0.) && shown.size.height > px(0.) {
                slot.surface.show(Some(shown), scale);
                let position = (
                    device(bounds.origin.x - shown.origin.x, scale),
                    device(bounds.origin.y - shown.origin.y, scale),
                );
                if slot.position != Some(position) {
                    slot.position = Some(position);
                    slot.view.move_to(position.0, position.1, true);
                }
            } else {
                slot.surface.show(None, scale);
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

    pub(crate) fn paint(
        &mut self,
        _key: WebKey,
        _bounds: Bounds<Pixels>,
        _hitbox: &Hitbox,
        _window: &mut Window,
    ) {
    }

    pub(crate) fn end_frame(&mut self, touched: &HashSet<WebKey>, window: &mut Window) {
        let scale = window.scale_factor();
        for (key, slot) in &mut self.views {
            if !touched.contains(key) {
                slot.surface.show(None, scale);
            }
        }
    }

    pub(crate) fn retain(&mut self, keep: &dyn Fn(WebKey) -> bool) {
        let dropped: Vec<WebKey> = self
            .views
            .keys()
            .copied()
            .filter(|key| !keep(*key))
            .collect();
        release(
            dropped
                .iter()
                .filter_map(|key| self.views.remove(key))
                .collect(),
        );
    }

    pub(crate) fn hide_all(&mut self) {
        for slot in self.views.values_mut() {
            // The scale only matters for a surface being shown.
            slot.surface.show(None, 1.);
        }
    }

    pub(crate) fn copy(&self, key: WebKey) {
        if let Some(slot) = self.views.get(&key) {
            slot.view.copy();
        }
    }

    pub(crate) fn select_all(&self, key: WebKey) {
        if let Some(slot) = self.views.get(&key) {
            slot.view.select_all();
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        release(self.views.drain().map(|(_, slot)| slot).collect());
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

/// Drops `slots`: GPUI destroys a surface's window (and the GTK window in
/// it) when the surface is dropped, so the engine destroys the views first.
fn release(slots: Vec<Slot>) {
    let mut surfaces = Vec::with_capacity(slots.len());
    let views = slots
        .into_iter()
        .map(|slot| {
            surfaces.push(slot.surface);
            slot.view
        })
        .collect();
    linux::destroy_now(views);
    drop(surfaces);
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
