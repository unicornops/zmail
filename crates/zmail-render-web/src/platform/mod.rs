//! Platform backends. Only macOS has one.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) mod macos;
