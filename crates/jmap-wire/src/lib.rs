//! A JMAP client library ([RFC 8620] core, [RFC 8621] mail) in layers,
//! each usable without the ones above it:
//!
//! 1. **The protocol core**, independent of any HTTP library:
//!    [`Request`] and [`Response`] envelopes, method calls with call ids
//!    and [result references](ResultReference), the [`Session`] object,
//!    and the three kinds of error a server reports
//!    ([`ProblemDetails`], [`MethodError`], [`SetError`]).
//! 2. **Typed calls**: [`Get`], [`Set`], [`Changes`], [`Query`] and
//!    [`QueryChanges`] for any data type ([`Object`]), and the data
//!    types of [`mail`] implemented so far, read into records that
//!    tolerate what servers leave out, null or add. Anything without a
//!    type is called by name with JSON arguments
//!    ([`Request::call_raw`]), in the same request as typed calls.
//! 3. **A client** ([`Client`]) over a small transport trait
//!    ([`HttpTransport`]), with [`Credentials`], a [`RedirectPolicy`]
//!    the caller chooses, a timeout, a response size limit and gzip
//!    (feature `gzip`). The default transport is [`ReqwestTransport`]
//!    (feature `reqwest`). Both features are on by default.
//!
//! # `using` holds what the request uses
//!
//! A request's `using` lists Core and the capabilities of the method
//! calls in it, nothing more (see [`Request`]). A server must reject a
//! request that names a capability it does not support (RFC 8620
//! section 3.6.1), so listing every capability a client library knows
//! breaks it against servers that implement only some.
//!
//! # Example
//!
//! Reading mail the idiomatic JMAP way: a query and a get of what it
//! finds, in one request (RFC 8620 section 3.7).
//!
#![cfg_attr(feature = "reqwest", doc = "```no_run")]
#![cfg_attr(not(feature = "reqwest"), doc = "```ignore")]
//! use jmap_wire::{
//!     Client, Comparator, Credentials, Get, Query, Request, capability,
//!     mail::{Email, EmailFilterCondition, EmailProperty},
//! };
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::builder()
//!     .credentials(Credentials::basic("john@example.com", "app-password"))
//!     .connect("https://jmap.example.com/session".parse()?)
//!     .await?;
//! let account = client.primary_account(capability::MAIL)?;
//!
//! let mut request = Request::new();
//! let query = request.call(
//!     Query::<Email>::new(account)
//!         .filter(EmailFilterCondition::new().has_keyword("$flagged"))
//!         .sort([Comparator::new("receivedAt").descending()])
//!         .limit(10),
//! )?;
//! let emails = request.call(
//!     Get::<Email>::new(account)
//!         .ids_from(query.ids())
//!         .properties([EmailProperty::Subject, EmailProperty::ReceivedAt]),
//! )?;
//! // using: core and mail, and no other.
//! assert_eq!(request.using().len(), 2);
//!
//! let mut response = client.send(&request).await?;
//! for email in response.take(&emails)?.list {
//!     println!("{:?} {:?}", email.received_at, email.subject);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # What a server returns is read leniently
//!
//! The records of [`mail`] have every property optional, keep the
//! properties this crate has no field for, and read a value that is not
//! of the type the RFC gives it as `None`, keeping its JSON. Servers
//! differ on `null` against leaving a property out, and now and then on
//! a type; one odd value then costs one property of one record instead
//! of the whole answer.
//!
//! # Without the default transport
//!
//! With `default-features = false` the crate has no HTTP library and no
//! async runtime among its dependencies. Implement [`HttpTransport`] to
//! bring your own, or use the envelope types alone: [`Request`]
//! serialises to the JSON to send, and [`Response`] deserialises from
//! what comes back, over any transport (the WebSocket binding of RFC
//! 8887 carries the same objects).
//!
//! # Dates
//!
//! Dates are kept as the RFC 3339 text the server sent
//! ([`mail::UtcDate`]); the crate depends on no date library.
//!
//! [RFC 8620]: https://www.rfc-editor.org/rfc/rfc8620
//! [RFC 8621]: https://www.rfc-editor.org/rfc/rfc8621

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod auth;
mod batch;
pub mod capability;
mod changes;
mod client;
pub mod discovery;
mod error;
pub mod mail;
mod method;
mod query;
mod record;
mod request;
#[cfg(feature = "reqwest")]
mod reqwest_transport;
mod response;
mod session;
mod transport;

pub use auth::{Credentials, TokenError, TokenRequest, TokenSource};
pub use batch::IdBatches;
pub use changes::{Changes, ChangesObject, ChangesResponse};
pub use client::{
    Client, ClientBuilder, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_MAX_SESSION_BYTES, DEFAULT_TIMEOUT,
    DEFAULT_USER_AGENT,
};
pub use error::{
    Error, MethodError, MethodErrorType, ProblemDetails, ProblemKind, RedirectRefusal, Result,
    SetError, SetErrorType, TransportError, TransportErrorKind,
};
pub use method::{
    Get, GetObject, GetResponse, Method, NoArguments, Object, PatchObject, Properties, Set,
    SetArguments, SetObject, SetResponse,
};
pub use query::{
    AddedItem, Comparator, Filter, Operator, Query, QueryChanges, QueryChangesResponse,
    QueryObject, QueryResponse,
};
pub use request::{Call, IdsReference, Invocation, Raw, Request, ResultReference};
#[cfg(feature = "reqwest")]
pub use reqwest_transport::ReqwestTransport;
pub use response::Response;
pub use session::{Account, CoreCapabilities, Session};
pub use transport::{
    HttpMethod, HttpRequest, HttpResponse, HttpTransport, RedirectAttempt, RedirectPolicy,
};

/// The README's examples, compiled as doctests.
#[cfg(all(doctest, feature = "reqwest"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
