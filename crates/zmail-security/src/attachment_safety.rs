//! Safety checks for received attachments: a file name that is safe to
//! write to disk, and whether a file can run code when opened.
//!
//! A received attachment's name comes from the sender. Before it names a
//! file on disk it goes through [`sanitize_filename`], and before the
//! reader hands a file to the operating system to open, [`open_risk`]
//! decides whether the user is warned first.

/// What a file is called when its name sanitises to nothing.
pub const FALLBACK_FILENAME: &str = "attachment";

/// Longest sanitised name, in bytes. Most file systems allow 255.
const MAX_FILENAME_BYTES: usize = 255;

/// Device names Windows reserves in every directory, with or without an
/// extension (`CON`, `con.txt`, `LPT1.log`).
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "COM¹", "COM²", "COM³", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "LPT¹", "LPT²", "LPT³", "CONIN$", "CONOUT$",
];

/// Turns an attachment's name into one file name that is safe on macOS,
/// Linux and Windows:
///
/// - path separators (`/`, `\`) and the other characters Windows forbids
///   (`<>:"|?*`) become `_`, so the name can never leave its directory;
/// - control characters and the invisible bidirectional and zero-width
///   marks are removed, so `invoice\u{202E}fdp.exe` cannot pose as a PDF;
/// - leading and trailing dots and spaces are trimmed, so `..` and `.`
///   are gone, the file is not hidden, and Windows does not drop a
///   trailing `.` from `setup.exe.`;
/// - Windows device names (`CON`, `NUL`, `COM1`, …) get a leading `_`;
/// - the name is cut to 255 bytes, keeping its extension;
/// - an empty result becomes [`FALLBACK_FILENAME`].
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|&ch| !ch.is_control() && !is_invisible_format_char(ch))
        .map(|ch| match ch {
            '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|ch: char| ch == '.' || ch.is_whitespace());
    if trimmed.is_empty() {
        return FALLBACK_FILENAME.to_owned();
    }

    let mut sanitized = trimmed.to_owned();
    let stem = sanitized.split('.').next().unwrap_or_default().trim_end();
    if WINDOWS_RESERVED_NAMES
        .iter()
        .any(|reserved| reserved.eq_ignore_ascii_case(stem))
    {
        sanitized.insert(0, '_');
    }
    truncate_keeping_extension(&sanitized, MAX_FILENAME_BYTES)
}

/// Bidirectional overrides, isolates and marks, and zero-width characters:
/// they change how a name looks without being visible themselves.
pub(crate) fn is_invisible_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Cuts `name` to at most `max` bytes on a character boundary, keeping a
/// short extension (up to 16 bytes) intact.
fn truncate_keeping_extension(name: &str, max: usize) -> String {
    if name.len() <= max {
        return name.to_owned();
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && extension.len() < 16 => {
            (stem, Some(extension))
        }
        _ => (name, None),
    };
    let budget = max - extension.map_or(0, |extension| extension.len() + 1);
    let mut end = budget.min(stem.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    let stem = stem[..end].trim_end_matches(|ch: char| ch == '.' || ch.is_whitespace());
    match extension {
        Some(extension) => format!("{stem}.{extension}"),
        None => stem.to_owned(),
    }
}

/// Extensions of files that run code, install software, or mount or link
/// to something else when opened, on any of macOS, Windows and Linux.
const EXECUTABLE_EXTENSIONS: &[&str] = &[
    // Windows programs, installers and shortcuts.
    "exe",
    "com",
    "scr",
    "pif",
    "cpl",
    "msi",
    "msp",
    "mst",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appinstaller",
    "application",
    "appref-ms",
    "gadget",
    "dll",
    "sys",
    "drv",
    "ocx",
    "lnk",
    "url",
    "scf",
    "inf",
    "reg",
    "msc",
    "hta",
    "chm",
    "settingcontent-ms",
    // Windows scripts.
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "psd1",
    "ps1xml",
    "psc1",
    "vb",
    "vbs",
    "vbe",
    "js",
    "jse",
    "mjs",
    "wsf",
    "wsh",
    "wsc",
    "sct",
    "xll",
    // Disk images, which mount and can carry programs past download checks.
    "iso",
    "img",
    "vhd",
    "vhdx",
    "dmg",
    // macOS programs, installers and scripts.
    "app",
    "pkg",
    "mpkg",
    "command",
    "tool",
    "workflow",
    "action",
    "terminal",
    "scpt",
    "scptd",
    "applescript",
    "osax",
    "prefpane",
    "webloc",
    "inetloc",
    "fileloc",
    // Linux and Unix programs, packages and scripts.
    "sh",
    "bash",
    "zsh",
    "csh",
    "ksh",
    "fish",
    "run",
    "bin",
    "out",
    "elf",
    "appimage",
    "flatpakref",
    "snap",
    "deb",
    "rpm",
    "desktop",
    // Interpreted scripts and cross-platform programs.
    "jar",
    "jnlp",
    "py",
    "pyw",
    "pyc",
    "pl",
    "rb",
    "php",
    "lua",
    "tcl",
    // Office documents that carry macros.
    "docm",
    "dotm",
    "xlsm",
    "xltm",
    "xlam",
    "pptm",
    "potm",
    "ppam",
    "ppsm",
    "sldm",
];

/// MIME types of programs, installers and scripts, for attachments whose
/// name has no telling extension.
const EXECUTABLE_CONTENT_TYPES: &[&str] = &[
    "application/x-msdownload",
    "application/x-msdos-program",
    "application/x-ms-installer",
    "application/x-msi",
    "application/x-ms-shortcut",
    "application/vnd.microsoft.portable-executable",
    "application/x-dosexec",
    "application/x-executable",
    "application/x-elf",
    "application/x-mach-binary",
    "application/x-sharedlib",
    "application/x-sh",
    "application/x-shellscript",
    "application/x-csh",
    "application/x-bat",
    "application/x-msdos-batch",
    "application/x-powershell",
    "application/javascript",
    "application/x-javascript",
    "text/javascript",
    "application/ecmascript",
    "text/ecmascript",
    "application/x-vbscript",
    "text/vbscript",
    "application/hta",
    "application/java-archive",
    "application/x-java-archive",
    "application/x-java-jnlp-file",
    "application/x-apple-diskimage",
    "application/x-iso9660-image",
    "application/vnd.apple.installer+xml",
    "application/x-debian-package",
    "application/vnd.debian.binary-package",
    "application/x-rpm",
    "application/x-redhat-package-manager",
    "application/x-desktop",
    "application/x-python",
    "text/x-python",
    "text/x-shellscript",
    "application/x-perl",
    "text/x-perl",
    "application/x-ruby",
    "application/vnd.ms-word.document.macroenabled.12",
    "application/vnd.ms-excel.sheet.macroenabled.12",
    "application/vnd.ms-excel.addin.macroenabled.12",
    "application/vnd.ms-powerpoint.presentation.macroenabled.12",
];

/// Extensions of web pages and vector images, which open in a browser and
/// can run scripts there. Phishing mail sends them as fake sign-in pages.
const ACTIVE_WEB_EXTENSIONS: &[&str] = &[
    "html", "htm", "xhtml", "xht", "shtml", "mht", "mhtml", "svg", "svgz",
];

/// MIME types of web pages and vector images, for attachments whose name
/// has no telling extension.
const ACTIVE_WEB_CONTENT_TYPES: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "application/x-mimearchive",
    "multipart/related",
];

/// What opening an attachment risks, from the most to the least serious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRisk {
    /// A program, installer, script, shortcut, disk image or
    /// macro-enabled document ([`is_executable_or_script`]).
    Program,
    /// A web page or vector image, which opens in a browser and can run
    /// scripts there.
    ActiveContent,
    /// Nothing that runs code when opened.
    None,
}

/// How risky it is to open the attachment. Anything but
/// [`OpenRisk::None`] gets a warning before the reader opens it.
pub fn open_risk(filename: &str, content_type: &str) -> OpenRisk {
    if is_executable_or_script(filename, content_type) {
        return OpenRisk::Program;
    }
    let (extension, mime) = extension_and_mime(filename, content_type);
    if ACTIVE_WEB_EXTENSIONS.contains(&extension.as_str())
        || ACTIVE_WEB_CONTENT_TYPES.contains(&mime.as_str())
    {
        OpenRisk::ActiveContent
    } else {
        OpenRisk::None
    }
}

/// The lower-case extension of the sanitised name (empty without one) and
/// the lower-case MIME type without parameters.
fn extension_and_mime(filename: &str, content_type: &str) -> (String, String) {
    let name = sanitize_filename(filename).to_ascii_lowercase();
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_owned())
        .unwrap_or_default();
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    (extension, mime)
}

/// Whether opening the attachment could run code: a program, installer,
/// script, shortcut, disk image or macro-enabled Office document, judged
/// by the extension of its sanitised name or by its MIME type. The reader
/// warns before opening one of these.
pub fn is_executable_or_script(filename: &str, content_type: &str) -> bool {
    let (extension, mime) = extension_and_mime(filename, content_type);
    EXECUTABLE_EXTENSIONS.contains(&extension.as_str())
        || EXECUTABLE_CONTENT_TYPES.contains(&mime.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_names_are_kept() {
        for name in [
            "report.pdf",
            "Q4 roadmap (draft).docx",
            "photo 2026-03-24.jpeg",
            "résumé.pdf",
            "日本語のファイル.txt",
            "archive.tar.gz",
        ] {
            assert_eq!(sanitize_filename(name), name);
        }
    }

    #[test]
    fn path_separators_and_traversal_cannot_leave_the_directory() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(
            sanitize_filename("..\\..\\Windows\\System32\\evil.dll"),
            "_.._Windows_System32_evil.dll"
        );
        assert_eq!(
            sanitize_filename("/absolute/path.txt"),
            "_absolute_path.txt"
        );
        assert_eq!(sanitize_filename("C:\\temp\\a.txt"), "C__temp_a.txt");
        for name in ["..", ".", "...", " . ", ""] {
            assert_eq!(sanitize_filename(name), FALLBACK_FILENAME, "{name:?}");
        }
        for name in [
            "../../etc/passwd",
            "..\\..\\x",
            "a/../../b",
            "/",
            "\\\\server\\share\\x",
        ] {
            let sanitized = sanitize_filename(name);
            assert!(!sanitized.contains('/') && !sanitized.contains('\\'));
            assert_ne!(sanitized, "..");
            assert!(!sanitized.starts_with('.'), "{sanitized}");
        }
    }

    #[test]
    fn windows_forbidden_characters_are_replaced() {
        assert_eq!(
            sanitize_filename("a<b>c:d\"e|f?g*h.txt"),
            "a_b_c_d_e_f_g_h.txt"
        );
    }

    #[test]
    fn control_and_invisible_characters_are_removed() {
        assert_eq!(sanitize_filename("re\u{0}port\n.pdf"), "report.pdf");
        assert_eq!(sanitize_filename("tab\there.txt"), "tabhere.txt");
        // A right-to-left override (U+202E) after "invoice" would show
        // "invoice<RLO>fdp.exe" as "invoiceexe.pdf".
        assert_eq!(
            sanitize_filename("invoice\u{202E}fdp.exe"),
            "invoicefdp.exe"
        );
        assert_eq!(sanitize_filename("zero\u{200B}width.txt"), "zerowidth.txt");
        assert_eq!(sanitize_filename("\u{FEFF}bom.txt"), "bom.txt");
    }

    #[test]
    fn leading_dots_and_trailing_dots_and_spaces_are_trimmed() {
        assert_eq!(sanitize_filename(".bashrc"), "bashrc");
        assert_eq!(sanitize_filename("setup.exe."), "setup.exe");
        assert_eq!(sanitize_filename("setup.exe . . "), "setup.exe");
        assert_eq!(sanitize_filename("  spaced.txt  "), "spaced.txt");
    }

    #[test]
    fn windows_reserved_names_are_prefixed() {
        for name in [
            "CON",
            "con.txt",
            "Nul",
            "aux.tar.gz",
            "COM1.log",
            "lpt9",
            "PRN .txt",
        ] {
            let sanitized = sanitize_filename(name);
            assert!(sanitized.starts_with('_'), "{name} became {sanitized}");
        }
        for name in ["console.txt", "CONTRACT.pdf", "COM10.txt", "nullable.rs"] {
            assert_eq!(sanitize_filename(name), name);
        }
    }

    #[test]
    fn long_names_are_cut_on_a_character_boundary_keeping_the_extension() {
        let long = format!("{}.pdf", "é".repeat(300));
        let sanitized = sanitize_filename(&long);
        assert!(sanitized.len() <= MAX_FILENAME_BYTES);
        assert!(sanitized.ends_with(".pdf"));
        let no_extension = "x".repeat(400);
        assert_eq!(sanitize_filename(&no_extension).len(), MAX_FILENAME_BYTES);
    }

    #[test]
    fn programs_scripts_and_installers_are_executable() {
        for name in [
            "setup.exe",
            "INSTALL.MSI",
            "run.bat",
            "run.cmd",
            "profile.ps1",
            "install.sh",
            "Viewer.app",
            "Viewer.dmg",
            "tool.jar",
            "invoice.js",
            "invoice.vbs",
            "screensaver.scr",
            "shortcut.lnk",
            "invoice.pdf.exe",
            "invoice\u{202E}fdp.exe",
            "setup.exe.",
            "page.hta",
            "disk.iso",
            "budget.xlsm",
            "package.deb",
            "launcher.desktop",
            "script.py",
        ] {
            assert!(
                is_executable_or_script(name, "application/octet-stream"),
                "{name} should be flagged"
            );
        }
    }

    #[test]
    fn documents_media_and_archives_are_not_executable() {
        for (name, content_type) in [
            ("report.pdf", "application/pdf"),
            ("photo.jpeg", "image/jpeg"),
            ("notes.txt", "text/plain"),
            (
                "budget.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            ),
            ("archive.zip", "application/zip"),
            ("song.mp3", "audio/mpeg"),
            ("invite.ics", "text/calendar"),
            ("exe", "application/octet-stream"),
            ("README", "application/octet-stream"),
        ] {
            assert!(
                !is_executable_or_script(name, content_type),
                "{name} should not be flagged"
            );
        }
    }

    #[test]
    fn web_pages_and_svg_are_active_content() {
        for (name, content_type) in [
            ("login.html", "application/octet-stream"),
            ("Invoice.HTM", "application/octet-stream"),
            ("page.xhtml", "application/octet-stream"),
            ("saved.mht", "application/octet-stream"),
            ("logo.svg", "image/svg+xml"),
            ("logo.svgz", "application/octet-stream"),
            ("statement", "text/html; charset=utf-8"),
            ("diagram", "image/svg+xml"),
        ] {
            assert_eq!(
                open_risk(name, content_type),
                OpenRisk::ActiveContent,
                "{name} should be active content"
            );
        }
    }

    #[test]
    fn programs_outrank_active_content_and_documents_carry_no_risk() {
        assert_eq!(open_risk("setup.exe", "text/html"), OpenRisk::Program);
        assert_eq!(open_risk("page.hta", "text/html"), OpenRisk::Program);
        assert_eq!(open_risk("report.pdf", "application/pdf"), OpenRisk::None);
        assert_eq!(open_risk("photo.png", "image/png"), OpenRisk::None);
        assert_eq!(open_risk("archive.zip", "application/zip"), OpenRisk::None);
        assert!(!is_executable_or_script("login.html", "text/html"));
    }

    #[test]
    fn executable_content_types_are_flagged_whatever_the_name() {
        assert!(is_executable_or_script(
            "download",
            "application/x-msdownload"
        ));
        assert!(is_executable_or_script(
            "readme.txt",
            "application/x-sh; charset=utf-8"
        ));
        assert!(is_executable_or_script("thing", "Application/Java-Archive"));
    }
}
