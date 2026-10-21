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
//! - [`Manager::login`] signs in with SRP (`proton-srp`, with the modulus
//!   signature checked by rPGP), then [`Client::auth_2fa`] takes a TOTP
//!   code; [`HumanVerification`] covers code 9001.
//! - [`Salts::salt_for_key`], [`Keys::unlock`] and [`unlock`] turn the key
//!   password into unlocked user and address keys on rPGP (ADR 0004).

mod address;
mod attachment;
mod auth;
mod boolean;
mod client;
mod config;
mod cookies;
mod event;
mod header_types;
mod hv;
mod keyring;
mod label;
mod manager;
mod manager_auth;
mod message;
mod paging;
mod pinning;
mod response;
mod salt;
mod srp_modulus;
mod unlock;
mod user;

#[cfg(test)]
mod srp_vectors;
#[cfg(test)]
mod tests;

pub use address::{Address, AddressStatus, AddressType};
pub use attachment::Attachment;
pub use client::Client;
pub use config::{API_HOST_URL, AppVersion, ClientConfig};
pub use cookies::{CookieJar, StoredCookie};
pub use event::{AddressEvent, Event, EventAction, LabelEvent, MAX_COLLECTED_EVENTS, MessageEvent};
pub use header_types::Headers;
pub use hv::HumanVerification;
pub use keyring::{Key, KeyFlags, KeyRing, Keys, UnlockedKey};
pub use label::*;
pub use manager::{Manager, Status};
pub use manager_auth::{Auth, AuthInfo, PasswordMode, TwoFaInfo, TwoFaStatus};
pub use message::{EmailAddress, Message, MessageFilter, MessageFlags, MessageMetadata};
pub use paging::{MAX_PAGE_SIZE, fetch_paged};
pub use pinning::{PinMismatch, PinParseError, PinSet};
pub use response::{ApiError, Code, Error};
pub use salt::{Salt, SaltedKeyPass, Salts};
pub use unlock::{UnlockedAccount, key_password, unlock};
pub use user::{ProductUsedSpace, User};
