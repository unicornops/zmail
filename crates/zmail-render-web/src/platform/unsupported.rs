//! The backend for platforms without a web renderer. The host marks itself
//! unavailable there, so nothing is ever placed.

use std::{collections::HashSet, rc::Rc};

use anyhow::{Result, bail};
use gpui::{Bounds, Pixels, Window};

use crate::host::WebKey;
use crate::{WebDocument, WebEvent};

/// No web views.
#[derive(Default)]
pub(crate) struct Backend {}

impl Backend {
    pub(crate) fn place(
        &mut self,
        _key: WebKey,
        _document: &Rc<WebDocument>,
        _bounds: Bounds<Pixels>,
        _viewport: Bounds<Pixels>,
        _window: &mut Window,
        _on_event: Rc<dyn Fn(WebEvent)>,
    ) -> Result<()> {
        bail!("this platform has no web renderer")
    }

    pub(crate) fn end_frame(&mut self, _touched: &HashSet<WebKey>, _window: &mut Window) {}

    pub(crate) fn retain(&mut self, _keep: &dyn Fn(WebKey) -> bool) {}

    pub(crate) fn hide_all(&mut self) {}

    pub(crate) fn copy(&self, _key: WebKey) {}

    pub(crate) fn select_all(&self, _key: WebKey) {}
}
