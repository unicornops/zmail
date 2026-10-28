//! The macOS backend of [`crate::host::WebHost`]: one native composition
//! surface for the reader, with an AppKit clip view in it holding a
//! `WKWebView` per message. Safe code only; the AppKit and WebKit calls are
//! in `platform::macos`.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use anyhow::{Context as _, Result, bail};
use gpui::{App, Bounds, Hitbox, Pixels, Window, WindowCompositionSurface};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::host::WebKey;
use crate::platform::macos::{Clip, Frame, PlatformWebView};
use crate::{WebDocument, WebEvent, debug_log};

/// The composition surface and the web views in it.
#[derive(Default)]
pub(crate) struct Backend {
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
    /// The `WKWebView` takes its own mouse input from AppKit.
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
        _cx: &mut App,
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

    pub(crate) fn paint(
        &mut self,
        _key: WebKey,
        _bounds: Bounds<Pixels>,
        _hitbox: &Hitbox,
        _window: &mut Window,
    ) {
    }

    pub(crate) fn end_frame(&mut self, touched: &HashSet<WebKey>, _window: &mut Window) {
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

    pub(crate) fn retain(&mut self, keep: &dyn Fn(WebKey) -> bool) {
        self.views.retain(|key, _| keep(*key));
    }

    pub(crate) fn hide_all(&mut self) {
        for slot in self.views.values() {
            slot.view.set_hidden(true);
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
