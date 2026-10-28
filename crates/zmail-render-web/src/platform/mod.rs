//! Platform backends. Each platform with a web renderer provides a
//! `Backend` for [`crate::host::WebHost`] with the same methods: `place`,
//! `paint`, `end_frame`, `retain`, `hide_all`, `copy` and `select_all`,
//! and a `FORWARDS_INPUT` constant: whether GPUI must hand the page its
//! mouse input (a hosted visual or a picture gets none of its own) or the
//! native view takes it itself. macOS, Windows and Linux have one; elsewhere
//! `unsupported::Backend` does nothing, and [`crate::SUPPORTED`] is false
//! so the host never places a view.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) mod macos;
#[cfg(target_os = "macos")]
mod macos_host;
#[cfg(target_os = "macos")]
pub(crate) use macos_host::Backend;

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub(crate) mod windows;
#[cfg(target_os = "windows")]
mod windows_host;
#[cfg(target_os = "windows")]
pub(crate) use windows_host::Backend;

// Linux: the FFI module, and safe code for the host, the sandbox checks
// and the network wall.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub(crate) mod linux;
#[cfg(target_os = "linux")]
mod linux_host;
#[cfg(target_os = "linux")]
mod linux_proxy;
#[cfg(target_os = "linux")]
mod linux_sandbox;
#[cfg(target_os = "linux")]
pub(crate) use linux_host::Backend;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub(crate) use unsupported::Backend;
