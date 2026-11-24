//! JMAP over WebSocket (RFC 8887): the same requests and responses as
//! over HTTP, on one connection the server can also push state changes
//! down.
//!
//! Three pieces, each usable without the ones after it:
//!
//! - the messages of the subprotocol as text: [`request_message`],
//!   [`PushEnable`], [`push_disable_message`] to send, and
//!   [`WebSocketMessage::parse`] for what arrives;
//! - [`WebSocketTransport`] and [`WebSocket`], the two traits a
//!   WebSocket library is used through, so that this crate forces none
//!   on its users. The `tungstenite` feature has an implementation,
//!   `TungsteniteTransport`, on `tokio-tungstenite`;
//! - [`Client::connect_websocket`], which opens the connection the
//!   session advertises with the client's credentials, and
//!   [`WebSocketConnection`], which sends and receives on it.
//!
//! Requests may be answered in any order (RFC 8887 section 4.3.2), so a
//! response is matched to its request by the id
//! [`WebSocketConnection::send`] returns. A request-level error arrives
//! as a message ([`WebSocketMessage::RequestError`]) carrying the same
//! [`ProblemDetails`] an HTTP error status would, and leaves the
//! connection open.
//!
#![cfg_attr(feature = "tungstenite", doc = "```no_run")]
#![cfg_attr(not(feature = "tungstenite"), doc = "```ignore")]
//! use std::time::Duration;
//! use jmap_wire::{
//!     Get, Request, TungsteniteTransport,
//!     mail::Mailbox,
//!     websocket::{PushEnable, WebSocketMessage},
//! };
//!
//! # async fn example(
//! #     client: &jmap_wire::Client<impl jmap_wire::HttpTransport>,
//! # ) -> Result<(), jmap_wire::Error> {
//! let account = client.primary_account(jmap_wire::capability::MAIL)?;
//! let mut socket = client.connect_websocket(&TungsteniteTransport::new()).await?;
//! socket.enable_push(&PushEnable::new().data_types(["Email", "Mailbox"])).await?;
//!
//! let mut request = Request::new();
//! let mailboxes = request.call(Get::<Mailbox>::new(account))?;
//! let sent = socket.send(&request).await?;
//!
//! // Nothing for five minutes means the connection is dead.
//! while let Some(message) = socket.next(Some(Duration::from_secs(300))).await? {
//!     match message {
//!         WebSocketMessage::Response { request_id, mut response }
//!             if request_id.as_deref() == Some(sent.as_str()) =>
//!         {
//!             println!("{} mailboxes", response.take(&mailboxes)?.list.len());
//!         }
//!         WebSocketMessage::StateChange(change) => {
//!             if change.touches(account, ["Email", "Mailbox"]) {
//!                 // Call Email/changes and Mailbox/changes.
//!             }
//!         }
//!         WebSocketMessage::RequestError { problem, .. } => {
//!             return Err(jmap_wire::Error::Problem(Box::new(problem)));
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::{fmt, future::Future, time::Duration};

use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::{
    auth::TokenRequest,
    client::Client,
    error::{Error, ProblemDetails, TransportError},
    push::StateChange,
    request::Request,
    response::Response,
    session::WebSocketCapability,
    transport::{HttpTransport, without_query},
};

/// The subprotocol's name (RFC 8887 section 6.1), which the handshake
/// asks for and the server must confirm.
pub const SUBPROTOCOL: &str = "jmap";
/// The largest WebSocket frame accepted unless a transport is told
/// otherwise: 16 MiB. A message may be several frames; its own limit is
/// the client's
/// [`max_response_bytes`](crate::ClientBuilder::max_response_bytes).
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// What a [`WebSocketTransport`] is asked to open.
///
/// `Debug` prints header names without their values, and the URL
/// without its query string, as either may carry secrets.
#[derive(Clone)]
#[non_exhaustive]
pub struct WebSocketHandshake {
    /// The URL: always `ws` or `wss`.
    pub url: Url,
    /// The headers of the handshake request beside the ones the
    /// WebSocket protocol itself needs, names in lower case.
    /// `authorization` is among them when the client has credentials.
    pub headers: Vec<(String, String)>,
    /// The subprotocol to ask for in `Sec-WebSocket-Protocol`, and to
    /// insist the server confirms: [`SUBPROTOCOL`].
    pub subprotocol: &'static str,
    /// How long connecting and the handshake may take.
    pub timeout: Duration,
    /// The largest message to accept, in bytes, however many frames it
    /// comes in.
    pub max_message_bytes: usize,
    /// The largest single frame to accept, in bytes.
    pub max_frame_bytes: usize,
}

impl WebSocketHandshake {
    /// A handshake with `url` for the JMAP subprotocol, with no
    /// headers, the default timeout and the default limits; set the
    /// fields from there.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            headers: Vec::new(),
            subprotocol: SUBPROTOCOL,
            timeout: crate::DEFAULT_TIMEOUT,
            max_message_bytes: crate::DEFAULT_MAX_RESPONSE_BYTES,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        }
    }
}

impl fmt::Debug for WebSocketHandshake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebSocketHandshake")
            .field("url", &without_query(&self.url))
            .field(
                "headers",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("subprotocol", &self.subprotocol)
            .field("timeout", &self.timeout)
            .field("max_message_bytes", &self.max_message_bytes)
            .field("max_frame_bytes", &self.max_frame_bytes)
            .finish()
    }
}

/// Opens WebSocket connections for a [`Client`]. Implement it to use
/// another WebSocket library than the `tungstenite` feature's.
///
/// An implementation must:
///
/// - verify the server's certificate for a `wss` URL as an HTTPS client
///   would, and never fall back to an unencrypted connection;
/// - send the handshake's headers and its subprotocol, and fail unless
///   the server's answer names the same subprotocol (RFC 8887 section
///   4.2);
/// - **not follow redirects**: an answer that is not the upgrade is an
///   error carrying its status ([`TransportError::with_status`]), so
///   that the client can tell refused credentials (401) from a server
///   without a WebSocket (404);
/// - keep to the handshake's timeout and its message and frame limits;
/// - send no headers of its own that identify the user, and log neither
///   header values nor messages.
pub trait WebSocketTransport: Send + Sync {
    /// The connection this transport opens.
    type Socket: WebSocket;

    /// Connects and completes the handshake.
    fn connect(
        &self,
        handshake: WebSocketHandshake,
    ) -> impl Future<Output = Result<Self::Socket, TransportError>> + Send;
}

impl<T: WebSocketTransport + ?Sized> WebSocketTransport for &T {
    type Socket = T::Socket;

    fn connect(
        &self,
        handshake: WebSocketHandshake,
    ) -> impl Future<Output = Result<Self::Socket, TransportError>> + Send {
        (**self).connect(handshake)
    }
}

/// An open WebSocket connection, as far as JMAP needs one: text
/// messages either way.
pub trait WebSocket: Send {
    /// Sends one text message.
    fn send(&mut self, text: String) -> impl Future<Output = Result<(), TransportError>> + Send;

    /// The next text message, whole (fragments put together), or
    /// `None` once the connection is closed. Pings are answered and
    /// binary messages skipped without returning (RFC 8887 section
    /// 4.3.1 lets a client ignore them).
    ///
    /// `idle` is how long to wait for a message: when none arrives in
    /// that time the call fails with a
    /// [`Timeout`](crate::TransportErrorKind::Timeout) error, and `None` waits
    /// without limit. A message over the handshake's limits is a
    /// [`TooLarge`](crate::TransportErrorKind::TooLarge) error.
    ///
    /// The future may be dropped before it is done (to send in the
    /// meantime, say); no message may be lost by that.
    fn receive(
        &mut self,
        idle: Option<Duration>,
    ) -> impl Future<Output = Result<Option<String>, TransportError>> + Send;

    /// Closes the connection with a normal closing handshake.
    fn close(&mut self) -> impl Future<Output = Result<(), TransportError>> + Send;
}

/// A request as the text of a WebSocket message (RFC 8887 section
/// 4.3.2): the Request object with `"@type": "Request"` and, when `id`
/// is given, the `id` the server echoes in its response.
///
/// ```
/// use jmap_wire::{Request, websocket::request_message};
/// use serde_json::json;
///
/// let mut request = Request::new();
/// request.call_raw("Core/echo", json!({"hello": true}));
/// let text = request_message(&request, Some("R1"))?;
/// assert_eq!(
///     serde_json::from_str::<serde_json::Value>(&text).expect("JSON"),
///     json!({"@type": "Request", "id": "R1",
///            "using": ["urn:ietf:params:jmap:core"],
///            "methodCalls": [["Core/echo", {"hello": true}, "c0"]]})
/// );
/// # Ok::<(), jmap_wire::Error>(())
/// ```
///
/// # Errors
///
/// [`Error::InvalidResultReference`] for a result reference that
/// cannot resolve, as [`Client::send`] gives it.
pub fn request_message(request: &Request, id: Option<&str>) -> Result<String, Error> {
    #[derive(Serialize)]
    struct Message<'a> {
        #[serde(rename = "@type")]
        kind: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        id: Option<&'a str>,
        #[serde(flatten)]
        request: &'a Request,
    }
    request.check_references()?;
    serde_json::to_string(&Message {
        kind: "Request",
        id,
        request,
    })
    .map_err(Error::Encode)
}

/// A WebSocketPushEnable object (RFC 8887 section 4.3.5.2): asks the
/// server to push state changes on this connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[must_use]
pub struct PushEnable {
    data_types: Option<Vec<String>>,
    push_state: Option<String>,
}

impl PushEnable {
    /// Changes to every data type, with no earlier state to catch up
    /// from.
    pub fn new() -> Self {
        Self::default()
    }

    /// Only changes to these data types (`Email`, `Mailbox`...).
    pub fn data_types(mut self, types: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.data_types = Some(types.into_iter().map(Into::into).collect());
        self
    }

    /// The last `pushState` an earlier connection received
    /// ([`StateChange::push_state`]), so that the server sends what
    /// changed since.
    pub fn push_state(mut self, state: impl Into<String>) -> Self {
        self.push_state = Some(state.into());
        self
    }

    /// The object as the text of a WebSocket message.
    pub fn message(&self) -> String {
        let mut message = serde_json::json!({
            "@type": "WebSocketPushEnable",
            "dataTypes": self.data_types,
        });
        if let Some(state) = &self.push_state {
            message["pushState"] = Value::from(state.as_str());
        }
        message.to_string()
    }
}

/// The WebSocketPushDisable object (RFC 8887 section 4.3.5.3) as the
/// text of a WebSocket message.
pub fn push_disable_message() -> String {
    r#"{"@type":"WebSocketPushDisable"}"#.to_owned()
}

/// What a server sends over a JMAP WebSocket (RFC 8887 section 4.3).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum WebSocketMessage {
    /// The response to a request (section 4.3.3).
    Response {
        /// The `id` of the request it answers; `None` when the request
        /// had none.
        request_id: Option<String>,
        /// The response, as over HTTP.
        response: Response,
    },
    /// A pushed state change (section 4.3.5.1), sent once push is
    /// enabled.
    StateChange(StateChange),
    /// A request-level error (section 4.3.4): what an HTTP error status
    /// with problem details is over HTTP. The connection stays open.
    RequestError {
        /// The `id` of the request that failed; `None` when the server
        /// could not tell which (a message that was not JSON, say).
        request_id: Option<String>,
        /// What went wrong.
        problem: ProblemDetails,
    },
    /// A message that is none of these: not a JSON object, or one this
    /// crate cannot read as any of the three. RFC 8887 section 4.3.1
    /// lets a client ignore it. The text is kept.
    Unknown(String),
}

impl WebSocketMessage {
    /// Reads the text of a message. Which of the three objects it is
    /// follows from its `@type`; a server that leaves `@type` out is
    /// read by what the object has (`methodResponses`, `changed`, or a
    /// problem's `type`).
    pub fn parse(text: &str) -> Self {
        let unknown = || Self::Unknown(text.to_owned());
        let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(text) else {
            return unknown();
        };
        let kind = match object.remove("@type") {
            Some(Value::String(kind)) => match kind.as_str() {
                "Response" => "Response",
                "StateChange" => "StateChange",
                "RequestError" => "RequestError",
                _ => return unknown(),
            },
            Some(_) => return unknown(),
            None if object.contains_key("methodResponses") => "Response",
            None if object.contains_key("changed") => "StateChange",
            None if object.contains_key("type") => "RequestError",
            None => return unknown(),
        };
        let request_id = match object.remove("requestId") {
            Some(Value::String(id)) => Some(id),
            _ => None,
        };
        let value = Value::Object(object);
        match kind {
            "Response" => serde_json::from_value(value)
                .map(|response| Self::Response {
                    request_id,
                    response,
                })
                .unwrap_or_else(|_| unknown()),
            "StateChange" => serde_json::from_value(value)
                .map(Self::StateChange)
                .unwrap_or_else(|_| unknown()),
            _ => serde_json::from_value(value)
                .map(|problem| Self::RequestError {
                    request_id,
                    problem,
                })
                .unwrap_or_else(|_| unknown()),
        }
    }
}

impl<T: HttpTransport> Client<T> {
    /// The WebSocket URL the session advertises (RFC 8887 section 3),
    /// checked: `wss`, or `ws` when the session itself was fetched over
    /// plain HTTP (a server on loopback, say). An `https` or `http` URL
    /// is read as `wss` or `ws`.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when the server advertises no WebSocket, and
    /// [`Error::InvalidUrl`] for a URL of another scheme, or an
    /// unencrypted one when the session came over HTTPS: the
    /// credentials would cross the network in the clear, which the
    /// client refuses as it refuses a redirect from HTTPS to HTTP.
    pub fn websocket_url(&self) -> Result<Url, Error> {
        let capability = self
            .session()
            .websocket()
            .ok_or_else(|| Error::Session("WebSocket capability".to_owned()))?;
        websocket_url(self.session_url(), &capability)
    }

    /// Opens the WebSocket the session advertises (RFC 8887 section
    /// 4.2) over `transport`, authenticated as the client's HTTP
    /// requests are, for the `jmap` subprotocol.
    ///
    /// The credentials go to the URL the session names, as they go to
    /// its API, upload and download URLs; a handshake that is answered
    /// with a redirect fails, since nothing says the credentials may
    /// follow. A 401 gets one more attempt with a fresh token when the
    /// credentials can refresh. Messages are limited to the client's
    /// [`max_response_bytes`](crate::ClientBuilder::max_response_bytes)
    /// and frames to [`DEFAULT_MAX_FRAME_BYTES`]; connecting is under
    /// the client's [`timeout`](crate::ClientBuilder::timeout).
    ///
    /// Whether the server pushes on the connection is the session's
    /// `supportsPush` ([`Session::websocket`](crate::Session::websocket)).
    ///
    /// # Errors
    ///
    /// Those of [`websocket_url`](Self::websocket_url);
    /// [`Error::Http`] when the server answers the handshake with a
    /// status instead of the upgrade; [`Error::Transport`] when the
    /// connection or the handshake fails otherwise.
    pub async fn connect_websocket<W: WebSocketTransport>(
        &self,
        transport: &W,
    ) -> Result<WebSocketConnection<W::Socket>, Error> {
        let url = self.websocket_url()?;
        let mut token = TokenRequest::Current;
        loop {
            let mut handshake = WebSocketHandshake::new(url.clone());
            handshake.timeout = self.config.timeout;
            handshake.max_message_bytes = self.config.max_response_bytes;
            handshake.max_frame_bytes = DEFAULT_MAX_FRAME_BYTES.min(self.config.max_response_bytes);
            if let Some(user_agent) = &self.config.user_agent {
                handshake
                    .headers
                    .push(("user-agent".to_owned(), user_agent.clone()));
            }
            if let Some(authorization) = self.config.credentials.authorization(token).await? {
                handshake
                    .headers
                    .push(("authorization".to_owned(), authorization));
            }
            match transport.connect(handshake).await {
                Ok(socket) => return Ok(WebSocketConnection::new(socket)),
                Err(error)
                    if error.status() == Some(401)
                        && token == TokenRequest::Current
                        && self.config.credentials.can_refresh() =>
                {
                    token = TokenRequest::Refresh;
                }
                Err(error) => {
                    return Err(match error.status() {
                        Some(status) => Error::Http { status },
                        None => error.into(),
                    });
                }
            }
        }
    }
}

/// The capability's URL as one a WebSocket can be opened at.
fn websocket_url(session_url: &Url, capability: &WebSocketCapability) -> Result<Url, Error> {
    let invalid = |reason: &str| Error::InvalidUrl {
        url: capability
            .url
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_owned(),
        reason: reason.to_owned(),
    };
    let mut url = session_url
        .join(&capability.url)
        .map_err(|error| invalid(&error.to_string()))?;
    let scheme = match url.scheme() {
        "wss" | "https" => "wss",
        "ws" | "http" => "ws",
        _ => return Err(invalid("it is neither ws nor wss")),
    };
    if scheme == "ws" && session_url.scheme() == "https" {
        return Err(invalid(
            "it is not encrypted (ws) while the session is (https)",
        ));
    }
    if url.scheme() != scheme {
        // `Url` only changes between schemes of the same kind, which
        // http, https, ws and wss are.
        url.set_scheme(scheme)
            .map_err(|()| invalid("it is neither ws nor wss"))?;
    }
    url.set_fragment(None);
    Ok(url)
}

/// A JMAP WebSocket connection (RFC 8887): requests out, and responses,
/// state changes and request-level errors in.
///
/// One task owns it. To send while waiting for a message, give
/// [`next`](Self::next) an idle time and send when it runs out, or drop
/// its future (it loses nothing) and send.
#[derive(Debug)]
pub struct WebSocketConnection<S> {
    socket: S,
    next_id: u64,
}

impl<S: WebSocket> WebSocketConnection<S> {
    /// A connection over a socket that is already open and has
    /// completed the `jmap` handshake by other means than
    /// [`Client::connect_websocket`].
    pub fn new(socket: S) -> Self {
        Self { socket, next_id: 0 }
    }

    /// Sends a request and returns the id it was given (`r0`, `r1`...),
    /// which the server echoes as the `request_id` of its
    /// [`WebSocketMessage::Response`] or
    /// [`WebSocketMessage::RequestError`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidResultReference`], before anything is sent, and
    /// [`Error::Transport`] when the message cannot be sent.
    pub async fn send(&mut self, request: &Request) -> Result<String, Error> {
        let id = format!("r{}", self.next_id);
        let message = request_message(request, Some(&id))?;
        self.next_id += 1;
        self.socket.send(message).await?;
        Ok(id)
    }

    /// Asks the server to push state changes on this connection (RFC
    /// 8887 section 4.3.5.2). The server does not acknowledge it; a
    /// request sent after it is answered after it took effect, on a
    /// server that handles messages in order.
    pub async fn enable_push(&mut self, enable: &PushEnable) -> Result<(), Error> {
        Ok(self.socket.send(enable.message()).await?)
    }

    /// Asks the server to stop pushing on this connection (RFC 8887
    /// section 4.3.5.3).
    pub async fn disable_push(&mut self) -> Result<(), Error> {
        Ok(self.socket.send(push_disable_message()).await?)
    }

    /// The next message, or `None` once the connection is closed.
    ///
    /// `idle` is how long to wait for one: when none arrives in that
    /// time the call fails with a timeout ([`Error::is_timeout`]) and
    /// the connection is still usable, so a caller can send a request
    /// to see whether the server is there, and call again. `None`
    /// waits without limit.
    ///
    /// # Errors
    ///
    /// [`Error::Transport`]: the timeout above, a message over the size
    /// limit, or the connection breaking.
    pub async fn next(
        &mut self,
        idle: Option<Duration>,
    ) -> Result<Option<WebSocketMessage>, Error> {
        Ok(self
            .socket
            .receive(idle)
            .await?
            .map(|text| WebSocketMessage::parse(&text)))
    }

    /// Closes the connection with a normal closing handshake. Dropping
    /// the connection closes it without one.
    pub async fn close(&mut self) -> Result<(), Error> {
        Ok(self.socket.close().await?)
    }

    /// The socket underneath.
    pub fn socket(&mut self) -> &mut S {
        &mut self.socket
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn parsed(value: Value) -> WebSocketMessage {
        WebSocketMessage::parse(&value.to_string())
    }

    /// The exchange of RFC 8887 section 4.4.
    #[test]
    fn the_messages_of_rfc_8887_section_4_4_are_written_and_read() {
        let enable = PushEnable::new()
            .data_types(["Mailbox", "Email"])
            .push_state("aaa");
        assert_eq!(
            serde_json::from_str::<Value>(&enable.message()).expect("JSON"),
            json!({"@type": "WebSocketPushEnable", "dataTypes": ["Mailbox", "Email"],
                   "pushState": "aaa"})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&PushEnable::new().message()).expect("JSON"),
            json!({"@type": "WebSocketPushEnable", "dataTypes": null})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&push_disable_message()).expect("JSON"),
            json!({"@type": "WebSocketPushDisable"})
        );

        let WebSocketMessage::StateChange(change) = parsed(json!({
            "@type": "StateChange",
            "changed": {"a456": {"Mailbox": "d35ecb040aab"}},
            "pushState": "bbb"
        })) else {
            panic!("a state change");
        };
        assert_eq!(change.state("a456", "Mailbox"), Some("d35ecb040aab"));
        assert_eq!(change.push_state.as_deref(), Some("bbb"));
        assert!(change.extra.is_empty(), "{:?}", change.extra);

        let WebSocketMessage::Response {
            request_id,
            response,
        } = parsed(json!({
            "@type": "Response",
            "requestId": "R1",
            "methodResponses": [["Core/echo", {"hello": true, "high": 5}, "b3ff"]]
        }))
        else {
            panic!("a response");
        };
        assert_eq!(request_id.as_deref(), Some("R1"));
        assert_eq!(response.method_responses[0].name, "Core/echo");

        let WebSocketMessage::RequestError {
            request_id,
            problem,
        } = parsed(json!({
            "@type": "RequestError",
            "requestId": null,
            "type": "urn:ietf:params:jmap:error:notJSON",
            "status": 400,
            "detail": "The request did not parse as I-JSON."
        }))
        else {
            panic!("a request error");
        };
        assert_eq!(request_id, None);
        assert_eq!(problem.kind(), crate::ProblemKind::NotJson);
        assert_eq!(problem.status, Some(400));
        assert!(problem.extra.is_empty(), "{:?}", problem.extra);
    }

    #[test]
    fn messages_without_a_type_are_read_by_their_shape_and_others_kept() {
        assert!(matches!(
            parsed(json!({"methodResponses": [], "sessionState": "s1"})),
            WebSocketMessage::Response {
                request_id: None,
                ..
            }
        ));
        assert!(matches!(
            parsed(json!({"changed": {"a1": {"Email": "s2"}}})),
            WebSocketMessage::StateChange(_)
        ));
        assert!(matches!(
            parsed(json!({"type": "urn:ietf:params:jmap:error:limit", "requestId": "r3"})),
            WebSocketMessage::RequestError { request_id: Some(id), .. } if id == "r3"
        ));
        for text in [
            "The quick brown fox",
            "[]",
            "{}",
            r#"{"@type":"PushVerification"}"#,
            r#"{"@type":"Response","methodResponses":"none"}"#,
            r#"{"@type":"StateChange","changed":7}"#,
        ] {
            assert_eq!(
                WebSocketMessage::parse(text),
                WebSocketMessage::Unknown(text.to_owned())
            );
        }
    }

    #[test]
    fn a_request_without_an_id_has_none() {
        let mut request = Request::new();
        request.call_raw("Core/echo", json!({}));
        let text = request_message(&request, None).expect("encodes");
        let value: Value = serde_json::from_str(&text).expect("JSON");
        assert_eq!(value["@type"], "Request");
        assert!(value.get("id").is_none());
    }

    #[test]
    fn websocket_urls_are_never_less_secure_than_the_session() {
        let url = |session: &str, advertised: &str| {
            websocket_url(
                &Url::parse(session).expect("a URL"),
                &WebSocketCapability {
                    url: advertised.to_owned(),
                    supports_push: true,
                },
            )
            .map(String::from)
        };
        let session = "https://jmap.example.com/session";
        assert_eq!(
            url(session, "wss://ws.example.com/jmap/ws/#x").expect("wss"),
            "wss://ws.example.com/jmap/ws/"
        );
        assert_eq!(
            url(session, "https://jmap.example.com/ws").expect("https is wss"),
            "wss://jmap.example.com/ws"
        );
        assert_eq!(
            url(session, "/jmap/ws").expect("relative"),
            "wss://jmap.example.com/jmap/ws"
        );
        assert!(matches!(
            url(session, "ws://jmap.example.com/ws?token=s3cret"),
            Err(Error::InvalidUrl { url, .. }) if url == "ws://jmap.example.com/ws"
        ));
        assert!(url(session, "ftp://jmap.example.com/ws").is_err());
        assert_eq!(
            url(
                "http://127.0.0.1:4080/session",
                "ws://127.0.0.1:4080/jmap/ws"
            )
            .expect("ws"),
            "ws://127.0.0.1:4080/jmap/ws"
        );
    }
}
