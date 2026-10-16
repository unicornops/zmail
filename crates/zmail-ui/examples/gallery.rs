//! The Zmail component gallery: every UI component in one window.
//!
//! ```text
//! cargo run -p zmail-ui --example gallery
//! ZMAIL_GALLERY_APPEARANCE=light ZMAIL_GALLERY_PLATFORM=windows cargo run -p zmail-ui --example gallery
//! ```

use std::{borrow::Cow, fs, path::PathBuf};

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// Serves the workspace `assets/` directory from disk, so the example does
/// not need the embedded asset crate.
struct DiskAssets {
    root: PathBuf,
}

impl AssetSource for DiskAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match fs::read(self.root.join(path)) {
            Ok(bytes) => Ok(Some(Cow::Owned(bytes))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.root.join(path))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            names.push(format!("{}/{name}", path.trim_end_matches('/')).into());
        }
        Ok(names)
    }
}

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    zmail_ui::run_gallery(DiskAssets { root });
}
