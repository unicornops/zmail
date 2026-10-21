//! Proton Mail API client for Zmail.
//!
//! A Rust port of [go-proton-api](https://github.com/ProtonMail/go-proton-api)
//! at the revision Proton Mail Bridge v3.27.1 uses, with Bridge's TLS
//! pinning (issue #237, ADR 0004). Pins and ported files are listed in
//! `docs/upstream/proton.md` and `docs/upstream/proton-ports.toml`; each
//! ported file names its upstream source and licence at the top.
//!
//! Nothing here is reachable from the UI until the local store is
//! encrypted at rest (#139).
//!
//! - [`ClientConfig`] fixes the API host, the honest
//!   `x-pm-appversion: zmail@<version>` header and TLS trust: the
//!   platform's roots plus Bridge's SPKI pins.
//! - [`Manager`] owns the HTTP connection pool, the cookie jar, the
//!   connection status and the retry rules (429 and 503 honour
//!   `Retry-After`; failed connects back off).
//! - [`Client`] is one session: it sends `x-pm-uid` and the bearer token,
//!   and on 401 refreshes the session once and retries once.

mod auth;
mod client;
mod config;
mod cookies;
mod hv;
mod manager;
mod manager_auth;
mod paging;
mod pinning;
mod response;
mod srp_modulus;
mod user;

#[cfg(test)]
mod srp_vectors;
#[cfg(test)]
mod tests;

pub use client::Client;
pub use config::{API_HOST_URL, AppVersion, ClientConfig};
pub use cookies::{CookieJar, StoredCookie};
pub use hv::HumanVerification;
pub use manager::{Manager, Status};
pub use manager_auth::{Auth, AuthInfo, PasswordMode, TwoFaInfo, TwoFaStatus};
pub use paging::{MAX_PAGE_SIZE, fetch_paged};
pub use pinning::{PinMismatch, PinParseError, PinSet};
pub use response::{ApiError, Code, Error};
pub use user::{ProductUsedSpace, User};
