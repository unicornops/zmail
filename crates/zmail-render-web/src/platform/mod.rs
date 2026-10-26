//! Platform backends. Each platform with a web renderer provides a
//! `Backend` for [`crate::host::WebHost`] with the same methods: `place`,
//! `end_frame`, `retain`, `hide_all`, `copy` and `select_all`. Only macOS
//! has one; elsewhere `unsupported::Backend` does nothing, and
//! [`crate::SUPPORTED`] is false so the host never places a view.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
mod macos_host;
#[cfg(target_os = "macos")]
pub(crate) use macos_host::Backend;

#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(not(target_os = "macos"))]
pub(crate) use unsupported::Backend;
