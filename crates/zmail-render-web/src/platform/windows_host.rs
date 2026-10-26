//! The Windows backend of [`crate::host::WebHost`]: one native composition
//! surface for the reader's viewport, with a child surface in it per
//! message, each the root visual of a WebView2 composition controller.
//! Safe code only; the WebView2 and Win32 calls are in `platform::windows`.
//!
//! GPUI's Windows surfaces are DirectComposition visuals (Zed PR #62379,
//! `gpui_windows/src/directx_renderer.rs`): a native surface's platform
//! handle is its visual, clipped to the surface's bounds and placed between
//! GPUI's base and overlay visuals. The viewport surface clips to the
//! conversation list; each message's surface is its child, so a message
//! scrolled partly out of the list is cut off at the list's edges and never
//! covers the action bar, and Zmail's menus, dialogs and toasts draw above
//! every page.
//!
//! A hosted visual gets no input from Windows: GPUI's window receives the
//! mouse, and GPUI's hit testing decides whether the page is under it (a
//! menu above it wins). [`Backend::paint`] forwards what reaches a page:
//! buttons, moves, leaving, and sideways wheel turns (vertical turns scroll
//! the conversation, as on macOS). The page's cursor comes back the other
//! way. Keyboard focus stays with GPUI: Copy and Select All reach the page
//! through Zmail's commands, as on macOS.
//!
//! Message surfaces are kept and reused, because removing a surface needs
//! the window and dropping a web view does not have it. GPUI does not take
//! the children of a native surface out of its visual before adding them
//! back when it rebuilds the visual tree, which DirectComposition refuses;
//! until it does, the viewport's children are detached first, every time
//! a rebuild is due (see `detach_children`).

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

use anyhow::{Context as _, Result, bail};
use gpui::{
    Bounds, CursorStyle, DevicePixels, DispatchPhase, Hitbox, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Window,
    WindowCompositionSurface,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::core::IUnknown;

use crate::host::WebKey;
use crate::platform::windows::{
    MouseKeys, MouseKind, PlatformWebView, VisualTarget, detach_children, unavailable_reason,
};
use crate::{WebDocument, WebEvent, debug_log};

/// The composition surfaces and the web views in them.
#[derive(Default)]
pub(crate) struct Backend {
    viewport: Option<Viewport>,
    views: HashMap<WebKey, Slot>,
    /// Message surfaces no web view uses, for the next one.
    spare: Vec<Child>,
    /// A surface's geometry changed this frame, so GPUI rebuilds its visual
    /// tree before presenting.
    geometry_changed: bool,
}

struct Viewport {
    surface: WindowCompositionSurface,
    /// The surface's visual; replaced if GPUI recreates its compositor.
    visual: Rc<RefCell<IUnknown>>,
    hwnd: isize,
    bounds: Option<Bounds<Pixels>>,
    visible: bool,
}

/// A message's surface, inside the viewport's.
struct Child {
    surface: WindowCompositionSurface,
    visual: Rc<RefCell<IUnknown>>,
    bounds: Option<Bounds<DevicePixels>>,
    /// The web view drawing into it, which follows its visual if GPUI
    /// recreates its compositor.
    occupant: Rc<RefCell<Option<PlatformWebView>>>,
}

struct Slot {
    view: PlatformWebView,
    child: Child,
    shown: Rc<Shown>,
    document: Rc<WebDocument>,
    on_event: Rc<dyn Fn(WebEvent)>,
    input: Rc<RefCell<Input>>,
}

/// Whether a message's surface is drawn: once its page is measured (so no
/// half-laid-out page flashes), and while a body places it.
struct Shown {
    surface: WindowCompositionSurface,
    hidden: Cell<bool>,
    revealed: Cell<bool>,
    visible: Cell<Option<bool>>,
}

impl Shown {
    fn apply(&self) {
        let visible = !self.hidden.get() && self.revealed.get();
        if self.visible.replace(Some(visible)) == Some(visible) {
            return;
        }
        if let Err(error) = self
            .surface
            .platform_surface()
            .and_then(|surface| surface.set_visible(visible))
        {
            eprintln!("zmail: web renderer: could not show or hide a page: {error:#}");
        }
    }
}

/// The mouse as one page sees it.
#[derive(Default)]
struct Input {
    /// The pointer was over the page at the last move.
    hovered: bool,
    /// A button went down on the page and has not come up: the page keeps
    /// getting moves and the release, wherever the pointer goes.
    pressed: Option<MouseButton>,
    /// The page's cursor when GPUI last painted it.
    painted_cursor: u32,
}

impl Backend {
    /// A hosted visual has no input of its own.
    pub(crate) const FORWARDS_INPUT: bool = true;

    pub(crate) fn place(
        &mut self,
        key: WebKey,
        document: &Rc<WebDocument>,
        bounds: Bounds<Pixels>,
        viewport: Bounds<Pixels>,
        window: &mut Window,
        on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<()> {
        if let Some(reason) = unavailable_reason() {
            bail!(reason);
        }
        self.ensure_viewport(viewport, window)?;
        let replace = self.views.get(&key).is_none_or(|slot| {
            !Rc::ptr_eq(&slot.document, document) && *slot.document != **document
        });
        if replace {
            debug_log(format_args!("creating the web view for message {key}"));
            let slot = self.new_slot(document, window, on_event)?;
            if let Some(old) = self.views.insert(key, slot) {
                self.release(old);
            }
        }
        let scale = window.scale_factor();
        let device = bounds.to_device_pixels(scale);
        let slot = self.views.get_mut(&key).context("no web view")?;
        if slot.child.bounds != Some(device) {
            slot.child.surface.platform_surface()?.set_bounds(device)?;
            slot.child.bounds = Some(device);
            self.geometry_changed = true;
        }
        slot.view.set_scale(f64::from(scale));
        slot.view
            .set_size(device.size.width.0, device.size.height.0);
        let hidden = !bounds.intersects(&viewport);
        slot.view.set_hidden(hidden);
        slot.shown.hidden.set(hidden);
        slot.shown.apply();
        Ok(())
    }

    /// Creates the viewport surface on first use, and keeps it on `viewport`.
    fn ensure_viewport(&mut self, viewport: Bounds<Pixels>, window: &mut Window) -> Result<()> {
        if self.viewport.is_none() {
            let hwnd = match HasWindowHandle::window_handle(window)
                .map_err(|error| anyhow::anyhow!("no window handle: {error:?}"))?
                .as_raw()
            {
                RawWindowHandle::Win32(handle) => handle.hwnd.get(),
                _ => bail!("the window is not a Win32 window"),
            };
            let composition = window
                .enable_window_composition()
                .context("enabling window composition")?;
            let surface = composition
                .create_native_surface()
                .context("creating the native surface")?;
            let visual = Rc::new(RefCell::new(surface_visual(&surface)?));
            follow_compositor(&surface, visual.clone(), None)?;
            debug_log(format_args!("native composition surface created"));
            self.viewport = Some(Viewport {
                surface,
                visual,
                hwnd,
                bounds: None,
                visible: true,
            });
        }
        let surface = self.viewport.as_mut().context("no surface")?;
        if surface.bounds != Some(viewport) {
            surface
                .surface
                .platform_surface()?
                .set_bounds(viewport.to_device_pixels(window.scale_factor()))?;
            surface.bounds = Some(viewport);
            self.geometry_changed = true;
        }
        Ok(())
    }

    /// A web view for `document` in a message surface.
    fn new_slot(
        &mut self,
        document: &Rc<WebDocument>,
        window: &mut Window,
        on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<Slot> {
        let child = match self.spare.pop() {
            Some(child) => child,
            None => self.new_child(window)?,
        };
        let hwnd = self.viewport.as_ref().context("no surface")?.hwnd;
        let shown = Rc::new(Shown {
            surface: child.surface.clone(),
            hidden: Cell::new(true),
            revealed: Cell::new(false),
            visible: Cell::new(None),
        });
        shown.apply();
        let reveal = Rc::downgrade(&shown);
        let forward = on_event.clone();
        let on_view_event: Rc<dyn Fn(WebEvent)> = Rc::new(move |event: WebEvent| {
            if matches!(event, WebEvent::Measured { .. })
                && let Some(shown) = reveal.upgrade()
                && !shown.revealed.replace(true)
            {
                shown.apply();
            }
            forward(event);
        });
        let target = VisualTarget::new(hwnd, child.visual.borrow().clone());
        let view = match PlatformWebView::new(&target, document, on_view_event) {
            Ok(view) => view,
            Err(error) => {
                self.spare.push(child);
                return Err(error);
            }
        };
        *child.occupant.borrow_mut() = Some(view.clone());
        Ok(Slot {
            view,
            child,
            shown,
            document: document.clone(),
            on_event,
            input: Rc::new(RefCell::new(Input::default())),
        })
    }

    /// A new message surface, made a child of the viewport's.
    fn new_child(&mut self, window: &mut Window) -> Result<Child> {
        let viewport = self.viewport.as_ref().context("no surface")?;
        let parent = viewport.surface.id();
        let parent_visual = viewport.visual.clone();
        let composition = window
            .enable_window_composition()
            .context("enabling window composition")?;
        // Both calls rebuild GPUI's visual tree, which re-adds the
        // viewport's children (see `detach_children`). The first try goes
        // without the workaround, so the log shows whether GPUI still needs
        // it.
        let surface = match composition.create_native_surface() {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!(
                    "zmail: web renderer: GPUI could not rebuild its visual tree with nested \
                     native surfaces ({error:#}); detaching them first"
                );
                detach_children(&parent_visual.borrow());
                composition
                    .create_native_surface()
                    .context("creating a message surface")?
            }
        };
        detach_children(&parent_visual.borrow());
        composition
            .reparent(surface.id(), Some(parent))
            .context("placing a message surface in the viewport")?;
        let visual = Rc::new(RefCell::new(surface_visual(&surface)?));
        let occupant: Rc<RefCell<Option<PlatformWebView>>> = Rc::new(RefCell::new(None));
        follow_compositor(&surface, visual.clone(), Some(occupant.clone()))?;
        debug_log(format_args!("message surface created"));
        Ok(Child {
            surface,
            visual,
            bounds: None,
            occupant,
        })
    }

    /// Closes a slot's web view and keeps its surface for the next one.
    fn release(&mut self, slot: Slot) {
        slot.shown.hidden.set(true);
        slot.shown.apply();
        slot.child.occupant.borrow_mut().take();
        drop(slot.view);
        self.spare.push(slot.child);
    }

    pub(crate) fn paint(
        &mut self,
        key: WebKey,
        bounds: Bounds<Pixels>,
        hitbox: &Hitbox,
        window: &mut Window,
    ) {
        let Some(slot) = self.views.get(&key) else {
            return;
        };
        let cursor = slot.view.cursor_id();
        slot.input.borrow_mut().painted_cursor = cursor;
        window.set_cursor_style(cursor_style(cursor), hitbox);
        let scale = window.scale_factor();
        let page_point = move |position: Point<Pixels>| {
            let offset = position - bounds.origin;
            (
                (f32::from(offset.x) * scale).round() as i32,
                (f32::from(offset.y) * scale).round() as i32,
            )
        };

        let (view, input, on_event, hitbox_down) = (
            slot.view.clone(),
            slot.input.clone(),
            slot.on_event.clone(),
            hitbox.clone(),
        );
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox_down.is_hovered(window) {
                return;
            }
            let Some(kind) = MouseKind::down(event.button, event.click_count) else {
                return;
            };
            input.borrow_mut().pressed = Some(event.button);
            let (x, y) = page_point(event.position);
            view.send_mouse(kind, keys(event.modifiers, Some(event.button)), 0, x, y);
            on_event(WebEvent::Pressed);
            cx.stop_propagation();
        });

        let (view, input, hitbox_up) = (slot.view.clone(), slot.input.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let ours = input.borrow().pressed == Some(event.button);
            if !ours && !hitbox_up.is_hovered(window) {
                return;
            }
            let Some(kind) = MouseKind::up(event.button) else {
                return;
            };
            input.borrow_mut().pressed = None;
            let (x, y) = page_point(event.position);
            view.send_mouse(kind, keys(event.modifiers, None), 0, x, y);
            if ours {
                cx.stop_propagation();
            }
        });

        let (view, input, hitbox_move) = (slot.view.clone(), slot.input.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let mut input = input.borrow_mut();
            let pressed = input.pressed.filter(|_| event.pressed_button.is_some());
            if hitbox_move.is_hovered(window) || pressed.is_some() {
                input.hovered = true;
                let (x, y) = page_point(event.position);
                view.send_mouse(MouseKind::Move, keys(event.modifiers, pressed), 0, x, y);
                // The page answers a move with its cursor; GPUI shows it at
                // the next paint.
                if view.cursor_id() != input.painted_cursor {
                    window.refresh();
                }
            } else if input.hovered {
                input.hovered = false;
                input.pressed = None;
                view.send_mouse(MouseKind::Leave, MouseKeys::default(), 0, 0, 0);
            }
        });

        let (view, hitbox_wheel) = (slot.view.clone(), hitbox.clone());
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox_wheel.is_hovered(window) {
                return;
            }
            // Vertical turns scroll the conversation (GPUI's list); sideways
            // ones scroll a page wider than the pane.
            let Some(delta) = horizontal_wheel(event.delta) else {
                return;
            };
            let (x, y) = page_point(event.position);
            view.send_mouse(
                MouseKind::HorizontalWheel,
                keys(event.modifiers, None),
                delta,
                x,
                y,
            );
            cx.stop_propagation();
        });
    }

    pub(crate) fn end_frame(&mut self, touched: &HashSet<WebKey>, _window: &mut Window) {
        for (key, slot) in &self.views {
            if !touched.contains(key) {
                slot.view.set_hidden(true);
                slot.shown.hidden.set(true);
                slot.shown.apply();
            }
        }
        self.set_visible(!touched.is_empty());
        if std::mem::take(&mut self.geometry_changed)
            && let Some(viewport) = &self.viewport
        {
            // GPUI rebuilds the tree when it presents this frame.
            detach_children(&viewport.visual.borrow());
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if let Some(viewport) = self.viewport.as_mut()
            && viewport.visible != visible
        {
            viewport.visible = visible;
            if let Err(error) = viewport
                .surface
                .platform_surface()
                .and_then(|surface| surface.set_visible(visible))
            {
                eprintln!("zmail: web renderer: could not show or hide the surface: {error:#}");
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
        for key in dropped {
            if let Some(slot) = self.views.remove(&key) {
                self.release(slot);
            }
        }
    }

    pub(crate) fn hide_all(&mut self) {
        for slot in self.views.values() {
            slot.view.set_hidden(true);
            slot.shown.hidden.set(true);
            slot.shown.apply();
        }
        self.set_visible(false);
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

/// A native surface's DirectComposition visual.
fn surface_visual(surface: &WindowCompositionSurface) -> Result<IUnknown> {
    surface
        .platform_surface()?
        .platform_handle()?
        .downcast::<IUnknown>()
        .map(|visual| *visual)
        .map_err(|_| anyhow::anyhow!("the native surface is not a DirectComposition visual"))
}

/// Keeps `visual` (and the web view in the surface) on the surface's
/// visual when GPUI recreates its compositor after a lost GPU device.
fn follow_compositor(
    surface: &WindowCompositionSurface,
    visual: Rc<RefCell<IUnknown>>,
    occupant: Option<Rc<RefCell<Option<PlatformWebView>>>>,
) -> Result<()> {
    surface
        .platform_surface()?
        .set_compositor_recreated_callback(Rc::new(move |handle| {
            let replacement = handle
                .downcast::<IUnknown>()
                .map(|visual| *visual)
                .map_err(|_| {
                    anyhow::anyhow!("the native surface is not a DirectComposition visual")
                })?;
            debug_log(format_args!(
                "compositor recreated: following the new visual"
            ));
            if let Some(view) = occupant
                .as_ref()
                .and_then(|occupant| occupant.borrow().clone())
            {
                view.retarget(&replacement);
            }
            *visual.borrow_mut() = replacement;
            Ok(())
        }))
}

/// The buttons and modifier keys held, as the page hears them.
fn keys(modifiers: gpui::Modifiers, pressed: Option<MouseButton>) -> MouseKeys {
    MouseKeys {
        left: pressed == Some(MouseButton::Left),
        right: pressed == Some(MouseButton::Right),
        middle: pressed == Some(MouseButton::Middle),
        shift: modifiers.shift,
        control: modifiers.control,
    }
}

/// A sideways wheel turn as a Windows wheel delta (120 a notch, positive to
/// the right), or `None` for a mostly vertical one. GPUI turns a notch into
/// three lines, or three columns for a sideways wheel.
fn horizontal_wheel(delta: ScrollDelta) -> Option<i32> {
    let (x, y, notch) = match delta {
        ScrollDelta::Lines(lines) => (lines.x, lines.y, 3.),
        // About 40 pixels a line.
        ScrollDelta::Pixels(pixels) => (f32::from(pixels.x), f32::from(pixels.y), 120.),
    };
    (x.abs() > y.abs() && x != 0.).then(|| (-x * 120. / notch).round() as i32)
}

/// GPUI's cursor for a Windows system cursor id (`IDC_*`) the page set.
fn cursor_style(id: u32) -> CursorStyle {
    match id {
        32513 => CursorStyle::IBeam,
        32515 => CursorStyle::Crosshair,
        32642 => CursorStyle::ResizeUpLeftDownRight,
        32643 => CursorStyle::ResizeUpRightDownLeft,
        32644 => CursorStyle::ResizeLeftRight,
        32645 => CursorStyle::ResizeUpDown,
        32648 => CursorStyle::OperationNotAllowed,
        32649 => CursorStyle::PointingHand,
        _ => CursorStyle::Arrow,
    }
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    #[test]
    fn only_sideways_turns_go_to_the_page() {
        assert_eq!(horizontal_wheel(ScrollDelta::Lines(point(0., 3.))), None);
        assert_eq!(
            horizontal_wheel(ScrollDelta::Lines(point(-3., 0.))),
            Some(120)
        );
        assert_eq!(
            horizontal_wheel(ScrollDelta::Lines(point(3., 1.))),
            Some(-120)
        );
        assert_eq!(
            horizontal_wheel(ScrollDelta::Pixels(point(px(-40.), px(0.)))),
            Some(40)
        );
    }

    #[test]
    fn page_cursors_map_to_gpui_cursors() {
        assert_eq!(cursor_style(32649), CursorStyle::PointingHand);
        assert_eq!(cursor_style(32513), CursorStyle::IBeam);
        assert_eq!(cursor_style(0), CursorStyle::Arrow);
    }
}
