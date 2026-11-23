//! Capability URIs, and which capability each method needs.
//!
//! A JMAP request names the capabilities it uses in `using` (RFC 8620
//! section 3.3), and a server must reject a request that names one it
//! does not support (`unknownCapability`, section 3.6.1). So a client
//! that lists every capability it knows about fails against any server
//! that implements fewer. This crate lists only what a request's own
//! method calls need; [`for_method`] is the table behind that for calls
//! made by name.

/// JMAP Core (RFC 8620): the request and response envelope, `Core/echo`,
/// `Blob/copy` and `PushSubscription`.
pub const CORE: &str = "urn:ietf:params:jmap:core";
/// JMAP Mail (RFC 8621 section 1.3.1): `Mailbox`, `Thread`, `Email` and
/// `SearchSnippet`.
pub const MAIL: &str = "urn:ietf:params:jmap:mail";
/// JMAP Submission (RFC 8621 section 1.3.2): `Identity` and
/// `EmailSubmission`.
pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
/// JMAP Vacation Response (RFC 8621 section 1.3.3): `VacationResponse`.
pub const VACATION_RESPONSE: &str = "urn:ietf:params:jmap:vacationresponse";
/// JMAP over WebSocket (RFC 8887 section 3.1). It describes a transport,
/// so no method call needs it in `using`.
pub const WEBSOCKET: &str = "urn:ietf:params:jmap:websocket";

/// The capability the methods of a data type need, for the data types of
/// RFC 8620 and RFC 8621; `None` for any other.
///
/// `method` is a method name such as `"Email/get"`; only the part before
/// the `/` is looked at.
///
/// ```
/// use jmap_wire::capability;
///
/// assert_eq!(capability::for_method("Email/query"), Some(capability::MAIL));
/// assert_eq!(capability::for_method("Identity/get"), Some(capability::SUBMISSION));
/// assert_eq!(capability::for_method("Core/echo"), Some(capability::CORE));
/// assert_eq!(capability::for_method("ContactCard/get"), None);
/// ```
pub fn for_method(method: &str) -> Option<&'static str> {
    let data_type = method.split('/').next().unwrap_or(method);
    match data_type {
        "Core" | "Blob" | "PushSubscription" => Some(CORE),
        "Mailbox" | "Thread" | "Email" | "SearchSnippet" => Some(MAIL),
        "Identity" | "EmailSubmission" => Some(SUBMISSION),
        "VacationResponse" => Some(VACATION_RESPONSE),
        _ => None,
    }
}
