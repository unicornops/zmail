//! JMAP Mail (RFC 8621): the data types implemented so far.
//!
//! - [`Mailbox`] (section 2): `Mailbox/get`, `/changes`, `/query`,
//!   `/queryChanges` and `/set`;
//! - [`Thread`] (section 3): `Thread/get` and `/changes`;
//! - [`Email`] (section 4): `Email/get` with header fields in their
//!   parsed forms and the body, `Email/query` with its filter and sort,
//!   `Email/changes`, `Email/queryChanges`, `Email/set` (creating with
//!   [`NewEmail`], keyword and mailbox changes with [`EmailUpdate`],
//!   destroying), `Email/import` ([`EmailImport`]) and `Email/copy`
//!   ([`CopyMethod`](crate::CopyMethod)`<Email>` with [`EmailCopy`]);
//! - [`Identity`] (section 6): `Identity/get` and `Identity/set`;
//! - [`EmailSubmission`] (section 7): `EmailSubmission/get` and
//!   `EmailSubmission/set`, with `onSuccessUpdateEmail` and
//!   `onSuccessDestroyEmail`.
//!
//! `Email/parse`, `SearchSnippet` and `VacationResponse` are to come;
//! until then, call them by name with
//! [`Request::call_raw`](crate::Request::call_raw). The messages and
//! attachments themselves travel as blobs:
//! [`Client::upload`](crate::Client::upload) and
//! [`Client::download`](crate::Client::download).
//!
//! # Records are lenient
//!
//! What a server returns is read into records whose properties are all
//! optional ([`MailboxRecord`], [`ThreadRecord`], [`EmailRecord`],
//! [`EmailBodyPart`], [`EmailBodyValue`]): a property that was not asked
//! for, that the server left out or that is `null` is `None`; one whose
//! value is not of the type the RFC gives it is `None` as well, with its
//! JSON kept in the record's `extra`; and properties this crate has no
//! field for are kept there too. No JSON object fails to decode as a
//! record.

mod email;
mod email_set;
mod header;
mod identity;
mod mailbox;
mod submission;
mod thread;

pub use email::{
    Email, EmailAddress, EmailAddressGroup, EmailBodyPart, EmailBodyValue, EmailFilterCondition,
    EmailHeader, EmailProperty, EmailQueryArguments, EmailRecord,
};
pub use email_set::{
    EmailCopy, EmailImport, EmailImportResponse, EmailToImport, EmailUpdate, NewBodyPart, NewEmail,
};
pub use header::{
    AsAddresses, AsDate, AsGroupedAddresses, AsMessageIds, AsRaw, AsText, AsUrls, HeaderForm,
    HeaderProperty, NotAHeaderProperty, ParsedForm,
};
pub use identity::{Identity, IdentityRecord, IdentityUpdate, NewIdentity};
pub use mailbox::{
    Mailbox, MailboxFilterCondition, MailboxQueryArguments, MailboxRecord, MailboxRights,
    MailboxSetArguments, MailboxUpdate, NewMailbox, Role,
};
pub use submission::{
    Address, DeliveryStatus, EmailSubmission, EmailSubmissionRecord, EmailSubmissionSetArguments,
    Envelope, NewEmailSubmission, UndoStatus,
};
pub use thread::{Thread, ThreadRecord};

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

impl From<&str> for UtcDate {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for UtcDate {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// A date and time with its own offset from UTC, as JMAP writes one: an
/// RFC 3339 `date-time` such as `2014-10-30T14:12:00+08:00` (RFC 8620
/// section 1.4). The `Date` header field of a message is one
/// (`sentAt`).
///
/// Like [`UtcDate`], it is kept as the text the server sent.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Date(String);

impl Date {
    /// A date from its RFC 3339 text, which is not checked.
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The date as RFC 3339 text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Date {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for Date {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}
