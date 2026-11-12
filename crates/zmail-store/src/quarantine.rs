//! Marking saved attachments as downloaded from the internet, so the
//! operating system's own checks apply when they are opened: Gatekeeper
//! on macOS (`com.apple.quarantine`), and SmartScreen and Office's
//! Protected View on Windows (a `Zone.Identifier` stream saying the
//! Internet zone, `ZoneId=3`). Linux has no such mark.
//!
//! Marking is best effort: a file system without extended attributes or
//! alternate data streams (FAT, many network shares) keeps the file
//! unmarked, and the save still succeeds.

use std::path::Path;

/// Marks `path` as downloaded. Errors are ignored: see the module
/// documentation.
pub(crate) fn mark_downloaded(path: &Path) {
    let _ = mark(path);
}

/// `flags;time;agent;event id`. 0x0081 is what browsers and Mail write
/// for a downloaded file the user has not yet approved; the event id
/// (which names a record in the quarantine database) is left empty.
#[cfg(target_os = "macos")]
fn mark(path: &Path) -> std::io::Result<()> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let value = format!("0081;{seconds:08x};Zmail;");
    rustix::fs::setxattr(
        path,
        QUARANTINE_ATTRIBUTE,
        value.as_bytes(),
        rustix::fs::XattrFlags::empty(),
    )?;
    Ok(())
}

/// The extended attribute macOS keeps the quarantine mark in.
#[cfg(target_os = "macos")]
pub(crate) const QUARANTINE_ATTRIBUTE: &str = "com.apple.quarantine";

/// The `Zone.Identifier` alternate data stream, as browsers write it.
#[cfg(windows)]
fn mark(path: &Path) -> std::io::Result<()> {
    std::fs::write(zone_identifier(path), ZONE_IDENTIFIER)
}

/// The Internet zone.
#[cfg(windows)]
pub(crate) const ZONE_IDENTIFIER: &str = "[ZoneTransfer]\r\nZoneId=3\r\n";

/// The path of `path`'s `Zone.Identifier` stream.
#[cfg(windows)]
pub(crate) fn zone_identifier(path: &Path) -> std::path::PathBuf {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    stream.into()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn mark(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
