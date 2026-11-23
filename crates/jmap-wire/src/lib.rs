//! A JMAP client library ([RFC 8620] core, [RFC 8621] mail) in layers,
//! each usable without the ones above it:
//!
//! 1. **The protocol core**, independent of any HTTP library:
//!    [`Request`] and [`Response`] envelopes, method calls with call ids
//!    and [result references](ResultReference), the [`Session`] object,
//!    and the three kinds of error a server reports
//!    ([`ProblemDetails`], [`MethodError`], [`SetError`]).
//! 2. **Typed calls**: [`Get`] and [`Set`] for any data type
//!    ([`Object`]), and the data types of [`mail`] implemented so far.
//!    Anything without a type is called by name with JSON arguments
//!    ([`Request::call_raw`]), in the same request as typed calls.
//! 3. **A client** ([`Client`]) over a small transport trait
//!    ([`HttpTransport`]), with [`Credentials`], a [`RedirectPolicy`]
//!    the caller chooses, a timeout and a response size limit. The
//!    default transport is [`ReqwestTransport`] (feature `reqwest`, on
//!    by default).
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
//! Two calls in one request, the second fed by the first, one typed and
//! one by name:
//!
#![cfg_attr(feature = "reqwest", doc = "```no_run")]
#![cfg_attr(not(feature = "reqwest"), doc = "```ignore")]
//! use jmap_wire::{Client, Credentials, Get, Request, capability, mail::Identity};
//! use serde_json::json;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::builder()
//!     .credentials(Credentials::basic("john@example.com", "app-password"))
//!     .connect("https://jmap.example.com/session".parse()?)
//!     .await?;
//! let account = client.primary_account(capability::MAIL)?;
//!
//! let mut request = Request::new();
//! let identities = request.call(Get::<Identity>::new(account))?;
//! let query = request.call_raw(
//!     "Email/query",
//!     json!({"accountId": account, "filter": {"hasKeyword": "$draft"}, "limit": 10}),
//! );
//! let drafts = request.call_raw(
//!     "Email/get",
//!     json!({
//!         "accountId": account,
//!         "#ids": query.result_reference("/ids"),
//!         "properties": ["subject", "receivedAt"]
//!     }),
//! );
//! // using: core, submission (Identity) and mail (Email), and no other.
//! assert_eq!(request.using().len(), 3);
//!
//! let response = client.send(&request).await?;
//! for identity in response.get(&identities)?.list {
//!     println!("{:?}", identity.email);
//! }
//! println!("{}", response.arguments(&drafts)?["list"]);
//! # Ok(())
//! # }
//! ```
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
