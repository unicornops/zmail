//! JMAP Mail (RFC 8621): the data types implemented so far.
//!
//! - [`Identity`] (section 6): `Identity/get` and `Identity/set`;
//! - [`EmailSubmission`] (section 7): `EmailSubmission/get` and
//!   `EmailSubmission/set`, with `onSuccessUpdateEmail` and
//!   `onSuccessDestroyEmail`;
//! - [`Email`] (section 4): `Email/set` only, and of the Email itself
//!   only its id; the rest of the type, with `Mailbox`, `Thread`,
//!   queries and changes, is to come. Until then, call those by name
//!   with [`Request::call_raw`](crate::Request::call_raw).

mod email;
mod identity;
mod submission;

pub use email::{Email, EmailAddress, NewEmail};
pub use identity::{Identity, IdentityRecord, IdentityUpdate, NewIdentity};
pub use submission::{
    Address, DeliveryStatus, EmailSubmission, EmailSubmissionRecord, EmailSubmissionSetArguments,
    Envelope, NewEmailSubmission, UndoStatus,
};

use serde::{Deserialize, Serialize};

/// A date and time in UTC, as JMAP writes one: an RFC 3339 `date-time`
/// with the `Z` zone, such as `2014-10-30T06:12:00Z` (RFC 8620 section
/// 1.4).
///
/// It is kept as the text the server sent. This crate does not parse
/// dates, so that it does not choose a date library for you; hand
/// [`as_str`](Self::as_str) to the one you use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UtcDate(String);

impl UtcDate {
    /// A date from its RFC 3339 text, which is not checked.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The date as RFC 3339 text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for UtcDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
