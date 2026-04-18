//! Compile-time embedded assets for Zmail (icons and other static files).

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};
use rust_embed::RustEmbed;

/// Embedded Lucide icon SVGs from the workspace-level `assets/icons/` directory.
///
/// Paths are prefixed with `icons/` so callers use paths like `"icons/mail.svg"`.
#[derive(RustEmbed)]
#[folder = "../../assets/icons"]
#[prefix = "icons/"]
struct Icons;

/// GPUI [`AssetSource`] that serves compile-time embedded assets.
///
/// Pass an instance to [`gpui::Application::with_assets`] before calling `.run()`:
///
/// ```rust,ignore
/// Application::new()
///     .with_assets(ZmailAssets)
///     .run(|cx| { /* … */ });
/// ```
pub struct ZmailAssets;

impl AssetSource for ZmailAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Icons::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Icons::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| SharedString::from(p.into_owned()))
            .collect())
    }
}
