//! The bundled UI font.
//!
//! macOS and Windows use the system UI font (`.SystemUIFont`: SF Pro and
//! Segoe UI Variable). GPUI's Linux text system maps `.SystemUIFont` to
//! IBM Plex Sans, which it does not ship, so text would fall back to
//! whatever fontconfig picks. Linux builds bundle Inter instead (it matches
//! GNOME's Adwaita Sans) and register it before the first window opens.
//!
//! Inter 4.1 (<https://github.com/rsms/inter/releases/tag/v4.1>), Copyright
//! The Inter Project Authors, licensed under the SIL Open Font License 1.1
//! (`assets/fonts/inter/LICENSE.txt`). Only the four static weights the type
//! scale uses are bundled: GPUI picks faces by their named weight, so a
//! variable font would render every weight as Regular.

use gpui::App;

use crate::styles::{PlatformStyle, UiFont};

/// The bundled Inter faces: Regular, Medium, SemiBold and Bold.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const INTER_FACES: [&[u8]; 4] = [
    include_bytes!("../../../assets/fonts/inter/Inter-Regular.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Medium.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/inter/Inter-Bold.ttf"),
];

/// Registers the platform's bundled UI font, if it has one. Call once at
/// startup, before opening a window. A failure is logged and the UI falls
/// back to the system's default sans-serif font.
pub(crate) fn register_ui_font(cx: &mut App) {
    let UiFont::Bundled(family) = PlatformStyle::current().ui_font else {
        return;
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let faces = INTER_FACES
            .iter()
            .map(|face| std::borrow::Cow::Borrowed(*face))
            .collect();
        if let Err(error) = cx.text_system().add_fonts(faces) {
            eprintln!("zmail: could not register the bundled {family} font: {error:#}");
        }
    }
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        // `PlatformStyle` only bundles a font on Linux.
        let _ = (family, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_bundles_inter_and_the_others_use_the_system_font() {
        assert_eq!(PlatformStyle::LINUX.ui_font, UiFont::Bundled("Inter"));
        assert_eq!(PlatformStyle::MACOS.ui_font, UiFont::System);
        assert_eq!(PlatformStyle::WINDOWS.ui_font, UiFont::System);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn the_bundled_faces_are_inter_truetype_files() {
        for face in INTER_FACES {
            // TrueType outlines (sfnt version 1.0), not a WOFF wrapper.
            assert_eq!(&face[..4], &[0, 1, 0, 0]);
            // The family name is stored in the name table as UTF-16BE.
            let family: Vec<u8> = "Inter".encode_utf16().flat_map(u16::to_be_bytes).collect();
            assert!(
                face.windows(family.len()).any(|window| window == family),
                "a bundled face is not named Inter"
            );
        }
    }
}
