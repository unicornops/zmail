//! The event source (RFC 8620 section 7.3): a long-lived response on
//! which the server sends a [`StateChange`] whenever data changes.
//!
//! Three pieces, each usable without the ones after it:
//!
//! - [`EventStreamParser`] reads the `text/event-stream` format of the
//!   [server-sent events specification][sse] from bytes, however they
//!   are cut into pieces;
//! - [`EventSourceEvent`] is what an event means in JMAP: a state
//!   change, a ping, or something else, kept;
//! - [`Client::event_source`] opens the stream over a
//!   [`StreamingTransport`] and [`EventStream::next`] reads it, failing
//!   when the server has been silent for longer than its pings allow.
//!
//! # Reconnecting is the caller's
//!
//! A stream that ends or fails is not reopened here. When to try again,
//! how long to wait and when to give up depend on things only the
//! application knows (whether the machine just woke, whether the user
//! is waiting), and a client that waits needs a timer, which this crate
//! does not have without a transport. What the crate gives a reconnect
//! is what the specification asks of one: the last event id
//! ([`EventStream::last_event_id`]) to send back
//! ([`EventSource::last_event_id`]) so that the server can send what was
//! missed, and the wait the server asked for
//! ([`EventStream::retry`]).
//!
#![cfg_attr(feature = "reqwest", doc = "```no_run")]
#![cfg_attr(not(feature = "reqwest"), doc = "```ignore")]
//! use std::time::Duration;
//! use jmap_wire::event_source::{EventSource, EventSourceEvent};
//!
//! # async fn example(
//! #     client: &jmap_wire::Client<jmap_wire::ReqwestTransport>,
//! #     wait: impl AsyncFn(Duration),
//! # ) -> Result<(), jmap_wire::Error> {
//! let account = client.primary_account(jmap_wire::capability::MAIL)?.to_owned();
//! let mut source = EventSource::new()
//!     .types(["Email", "Mailbox"])
//!     .ping(Duration::from_secs(30));
//! let mut delay = Duration::from_secs(1);
//! loop {
//!     match client.event_source(&source).await {
//!         Ok(mut events) => {
//!             // Nothing replays what changed while no stream was open
//!             // unless the server keeps event ids: fetch changes here.
//!             loop {
//!                 match events.next().await {
//!                     Ok(Some(EventSourceEvent::StateChange(change))) => {
//!                         delay = Duration::from_secs(1);
//!                         if change.touches(&account, ["Email", "Mailbox"]) {
//!                             // Call Email/changes and Mailbox/changes.
//!                         }
//!                     }
//!                     Ok(Some(_)) => {}
//!                     // Ended by the server, or failed (silence included).
//!                     Ok(None) | Err(_) => break,
//!                 }
//!             }
//!             if let Some(id) = events.last_event_id() {
//!                 source = source.last_event_id(id);
//!             }
//!             delay = events.retry().unwrap_or(delay);
//!         }
//!         // No event source at all: poll instead.
//!         Err(error) if error.http_status() == Some(404) => return Err(error),
//!         Err(_) => {}
//!     }
//!     wait(delay).await;
//!     delay = (delay * 2).min(Duration::from_secs(60));
//! }
//! # }
//! ```
//!
//! [sse]: https://html.spec.whatwg.org/multipage/server-sent-events.html

use std::{collections::VecDeque, time::Duration};

use serde::Deserialize;
use url::Url;

use crate::{
    client::{Client, Exchange, open, stream_success},
    error::{Error, TransportError, TransportErrorKind},
    push::StateChange,
    transport::{BodyStream, HttpMethod, StreamingTransport, is_media_type},
};

/// The longest line, and the most data in one event, that an
/// [`EventStreamParser`] holds unless told otherwise: 1 MiB. A
/// StateChange is a few hundred bytes.
pub const DEFAULT_MAX_EVENT_BYTES: usize = 1024 * 1024;
/// The ping interval an [`EventSource`] asks for unless
/// [`ping`](EventSource::ping) says otherwise: 30 seconds, the shortest
/// every server must allow (RFC 8620 section 7.3).
pub const DEFAULT_PING: Duration = Duration::from_secs(30);
/// How long after a ping was due a stream counts as dead, unless
/// [`ping_grace`](EventSource::ping_grace) says otherwise.
pub const DEFAULT_PING_GRACE: Duration = Duration::from_secs(15);
/// The shortest ping interval a server's announcement is taken at: a
/// server that says it pings more often than this is held to this.
const MIN_ANNOUNCED_PING: Duration = Duration::from_secs(5);

/// One event of an event stream, as the server-sent events
/// specification dispatches it (section 9.2.6, "Interpreting an event
/// stream").
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerSentEvent {
    /// The event's type: its `event` field, or `message` when it had
    /// none.
    pub event: String,
    /// Its `data` lines, joined with line feeds.
    pub data: String,
    /// The last event id of the stream as of this event: this event's
    /// `id` field, or that of an earlier event; `None` until one is
    /// seen, or after an empty `id` field reset it.
    pub id: Option<String>,
}

/// Reads `text/event-stream` from bytes as they arrive ([server-sent
/// events][sse], sections 9.2.5 "Parsing an event stream" and 9.2.6
/// "Interpreting an event stream").
///
/// It follows the specification's rules: a line ends at a line feed, a
/// carriage return, or the pair, wherever the bytes are cut; a line
/// that starts with a colon is a comment (servers send them to keep a
/// connection open); `data` fields add up, one per line; an event
/// without data is not dispatched; `id` sets the last event id unless
/// it contains a NUL; `retry` is taken only when it is all digits; a
/// byte order mark at the start is skipped; and what is not UTF-8 is
/// read with replacement characters. An event the stream ends in the
/// middle of is dropped.
///
/// What the specification leaves unbounded is bounded here: a line, or
/// the data of one event, over the limit is an error, since the bytes
/// come from a server.
///
/// ```
/// use jmap_wire::event_source::EventStreamParser;
///
/// let mut parser = EventStreamParser::new();
/// // Cut in the middle of a line, with a comment and a multi-line event.
/// assert!(parser.feed(b": keep-alive\n\nevent: sta")?.is_empty());
/// let events = parser.feed(b"te\r\nid: 7\r\ndata: one\r\ndata: two\r\n\r\n")?;
/// assert_eq!(events.len(), 1);
/// assert_eq!(events[0].event, "state");
/// assert_eq!(events[0].data, "one\ntwo");
/// assert_eq!(parser.last_event_id(), Some("7"));
/// # Ok::<(), jmap_wire::TransportError>(())
/// ```
///
/// [sse]: https://html.spec.whatwg.org/multipage/server-sent-events.html
#[derive(Debug, Clone)]
pub struct EventStreamParser {
    max_bytes: usize,
    /// The bytes of the line being read.
    line: Vec<u8>,
    /// The last byte was a carriage return: a line feed that follows
    /// belongs to it.
    after_carriage_return: bool,
    /// Nothing has been read yet: a byte order mark may come.
    at_start: bool,
    event: String,
    data: String,
    /// The last event id buffer: set by `id` fields, made the stream's
    /// last event id when an event is dispatched.
    id_buffer: Option<String>,
    last_event_id: Option<String>,
    retry: Option<Duration>,
}

impl Default for EventStreamParser {
    fn default() -> Self {
        Self::new()
    }
}

impl EventStreamParser {
    /// A parser with the default limit ([`DEFAULT_MAX_EVENT_BYTES`]).
    pub fn new() -> Self {
        Self::with_max_event_bytes(DEFAULT_MAX_EVENT_BYTES)
    }

    /// A parser that holds lines, and events' data, of at most
    /// `max_bytes`.
    pub fn with_max_event_bytes(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            line: Vec::new(),
            after_carriage_return: false,
            at_start: true,
            event: String::new(),
            data: String::new(),
            id_buffer: None,
            last_event_id: None,
            retry: None,
        }
    }

    /// Starts the parser off with the last event id of an earlier
    /// stream, so that events without an `id` report it.
    pub fn with_last_event_id(mut self, id: impl Into<String>) -> Self {
        let id = Some(id.into()).filter(|id| !id.is_empty());
        self.id_buffer = id.clone();
        self.last_event_id = id;
        self
    }

    /// Takes the next bytes of the stream and returns the events they
    /// complete.
    ///
    /// # Errors
    ///
    /// A [`TooLarge`](TransportErrorKind::TooLarge) error when a line or
    /// an event's data is over the limit. The parser should not be fed
    /// again after it.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ServerSentEvent>, TransportError> {
        let mut events = Vec::new();
        for &byte in bytes {
            if std::mem::take(&mut self.after_carriage_return) && byte == b'\n' {
                continue;
            }
            match byte {
                b'\n' | b'\r' => {
                    self.after_carriage_return = byte == b'\r';
                    let line = std::mem::take(&mut self.line);
                    if let Some(event) = self.line(&line)? {
                        events.push(event);
                    }
                }
                _ => {
                    if self.line.len() >= self.max_bytes {
                        return Err(self.too_large("line"));
                    }
                    self.line.push(byte);
                }
            }
        }
        Ok(events)
    }

    /// The stream's last event id: what a reconnect sends as
    /// `Last-Event-ID`. `None` when the server has sent none.
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    /// The reconnection time the server last asked for with a `retry`
    /// field.
    pub fn retry(&self) -> Option<Duration> {
        self.retry
    }

    fn too_large(&self, what: &str) -> TransportError {
        TransportError::new(
            TransportErrorKind::TooLarge,
            format!(
                "an event-stream {what} is larger than {} bytes",
                self.max_bytes
            ),
        )
    }

    /// One line of the stream, without its end.
    fn line(&mut self, line: &[u8]) -> Result<Option<ServerSentEvent>, TransportError> {
        let line = if std::mem::take(&mut self.at_start) {
            line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line)
        } else {
            line
        };
        if line.is_empty() {
            return Ok(self.dispatch());
        }
        let line = String::from_utf8_lossy(line);
        // A line that starts with a colon is a comment: its field name
        // is empty, which is no field.
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.data.len() + value.len() >= self.max_bytes {
                    return Err(self.too_large("event"));
                }
                self.data.push_str(value);
                self.data.push('\n');
            }
            "id" if !value.contains('\0') => {
                self.id_buffer = Some(value.to_owned()).filter(|id| !id.is_empty());
            }
            "retry" if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                if let Ok(milliseconds) = value.parse() {
                    self.retry = Some(Duration::from_millis(milliseconds));
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// The blank line that ends an event.
    fn dispatch(&mut self) -> Option<ServerSentEvent> {
        self.last_event_id.clone_from(&self.id_buffer);
        let event = std::mem::take(&mut self.event);
        let mut data = std::mem::take(&mut self.data);
        if data.is_empty() {
            return None;
        }
        // The line feed after the last data line is not part of it.
        data.pop();
        Some(ServerSentEvent {
            event: if event.is_empty() {
                "message".to_owned()
            } else {
                event
            },
            data,
            id: self.last_event_id.clone(),
        })
    }
}

/// What an event of the JMAP event source is.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventSourceEvent {
    /// Data changed (the `state` event of RFC 8620 section 7.3). An
    /// event without a type whose data is a StateChange is read as one
    /// too.
    StateChange(StateChange),
    /// The server's ping, with the interval it says it pings at.
    Ping {
        /// The `interval` of the ping's data; `None` when it had none
        /// that could be read.
        interval: Option<Duration>,
    },
    /// Any other event, as it came: one of another type, or a `state`
    /// event whose data is not a StateChange (not JSON, or an object of
    /// another `@type`). A caller that would rather sync once too often
    /// than miss a change can take an unreadable `state` event for one.
    Other(ServerSentEvent),
}

impl EventSourceEvent {
    /// What a server-sent event means on the JMAP event source.
    pub fn from_event(event: ServerSentEvent) -> Self {
        match event.event.as_str() {
            "ping" => {
                #[derive(Deserialize)]
                struct Ping {
                    interval: u64,
                }
                Self::Ping {
                    interval: serde_json::from_str::<Ping>(&event.data)
                        .ok()
                        .map(|ping| Duration::from_secs(ping.interval)),
                }
            }
            // RFC 8620 names the event `state`; a server that leaves the
            // event field out sends the default type.
            "state" | "message" => match StateChange::parse(&event.data) {
                Ok(Some(change)) => Self::StateChange(change),
                _ => Self::Other(event),
            },
            _ => Self::Other(event),
        }
    }
}

/// What to ask of the event source (RFC 8620 section 7.3): which data
/// types to hear about, whether the server should close the response
/// after each state event, how often it should ping, and where an
/// earlier stream left off.
///
/// ```
/// use std::time::Duration;
/// use jmap_wire::event_source::EventSource;
///
/// let source = EventSource::new()
///     .types(["Email", "Mailbox"])
///     .ping(Duration::from_secs(60))
///     .last_event_id("7");
/// # let _ = source;
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct EventSource {
    types: Vec<String>,
    close_after_state: bool,
    ping: Duration,
    ping_grace: Duration,
    idle_timeout: Option<Duration>,
    open_timeout: Option<Duration>,
    last_event_id: Option<String>,
    max_event_bytes: usize,
}

impl Default for EventSource {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSource {
    /// Every data type (`types=*`), a stream that stays open
    /// (`closeafter=no`) and a ping every 30 seconds.
    pub fn new() -> Self {
        Self {
            types: Vec::new(),
            close_after_state: false,
            ping: DEFAULT_PING,
            ping_grace: DEFAULT_PING_GRACE,
            idle_timeout: None,
            open_timeout: None,
            last_event_id: None,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
        }
    }

    /// Only these data types (`Email`, `Mailbox`...); none means all.
    pub fn types(mut self, types: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.types = types.into_iter().map(Into::into).collect();
        self
    }

    /// Has the server end the response after each state event
    /// (`closeafter=state`), for networks whose proxies hold a response
    /// back until it ends. [`EventStream::next`] then returns `None`
    /// after the event, and the caller opens the next stream.
    pub fn close_after_state(mut self) -> Self {
        self.close_after_state = true;
        self
    }

    /// How often the server should ping when it has nothing else to
    /// send, in whole seconds; zero for no pings. A server may use
    /// another interval (it must allow any between 30 and 300 seconds)
    /// and says which in each ping.
    ///
    /// The pings are what tells a dead connection from a quiet one: a
    /// stream on which nothing at all arrives (no event, no comment)
    /// for the interval plus the [grace](Self::ping_grace) fails with a
    /// timeout. Without pings there is no such check unless
    /// [`idle_timeout`](Self::idle_timeout) sets one.
    pub fn ping(mut self, interval: Duration) -> Self {
        self.ping = Duration::from_secs(interval.as_secs());
        self
    }

    /// How long after a ping was due the stream counts as dead;
    /// [`DEFAULT_PING_GRACE`] unless set.
    pub fn ping_grace(mut self, grace: Duration) -> Self {
        self.ping_grace = grace;
        self
    }

    /// How long the stream may be silent when no pings were asked for
    /// (`ping` of zero). With pings, the ping interval decides.
    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// How long the server may take to begin its response; the
    /// client's [`timeout`](crate::ClientBuilder::timeout) unless set.
    pub fn open_timeout(mut self, timeout: Duration) -> Self {
        self.open_timeout = Some(timeout);
        self
    }

    /// The last event id of an earlier stream
    /// ([`EventStream::last_event_id`]), sent as `Last-Event-ID` so
    /// that a server that keeps ids sends the changes since.
    pub fn last_event_id(mut self, id: impl Into<String>) -> Self {
        self.last_event_id = Some(id.into()).filter(|id| !id.is_empty());
        self
    }

    /// The longest line and the most data in one event that are held;
    /// [`DEFAULT_MAX_EVENT_BYTES`] unless set.
    pub fn max_event_bytes(mut self, bytes: usize) -> Self {
        self.max_event_bytes = bytes;
        self
    }

    /// The value of the URL's `types` variable.
    fn types_variable(&self) -> String {
        if self.types.is_empty() {
            "*".to_owned()
        } else {
            self.types.join(",")
        }
    }

    /// How long the stream may be silent when the server pings at
    /// `ping`.
    fn silence(&self, ping: Duration) -> Option<Duration> {
        if ping.is_zero() {
            self.idle_timeout
        } else {
            Some(ping + self.ping_grace)
        }
    }
}

impl<T: StreamingTransport> Client<T> {
    /// Opens the event source (RFC 8620 section 7.3): an authenticated
    /// GET of the session's `eventSourceUrl` with `source`'s variables
    /// filled in, asking for `text/event-stream`, with `Last-Event-ID`
    /// when `source` has one.
    ///
    /// The request is made like any other of the client's: with its
    /// credentials, its [`RedirectPolicy`](crate::RedirectPolicy)
    /// judged against the event source's host, and one more attempt
    /// with a fresh token after a 401 when the credentials can refresh.
    /// It asks for no compression.
    ///
    /// # Errors
    ///
    /// [`Error::Session`] when the session has no `eventSourceUrl`;
    /// [`Error::Problem`] or [`Error::Http`] when the server refuses
    /// (404 from one that has no event source);
    /// [`Error::NotEventStream`] when the answer is something else than
    /// an event stream; [`Error::Redirect`] for a redirect that is not
    /// followed; [`Error::Transport`] when no response begins within
    /// the timeout.
    pub async fn event_source(&self, source: &EventSource) -> Result<EventStream<T::Body>, Error> {
        let url = self.event_source_url(source)?;
        let mut extra_headers = vec![("cache-control".to_owned(), "no-cache".to_owned())];
        if let Some(id) = &source.last_event_id {
            // A header value cannot carry a line break; an id with one
            // cannot have come from an event stream.
            if id.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
                return Err(TransportError::new(
                    TransportErrorKind::Other,
                    "the last event id cannot be sent in an HTTP header",
                )
                .into());
            }
            extra_headers.push(("last-event-id".to_owned(), id.clone()));
        }
        let timeout = source.open_timeout.unwrap_or(self.config.timeout);
        let (response, _) = open(
            &self.transport,
            &self.config,
            Exchange {
                method: HttpMethod::Get,
                url,
                body: Vec::new(),
                content_type: None,
                accept: "text/event-stream",
                accept_gzip: false,
                timeout,
                max_response_bytes: 0,
                redirects_without_credentials: false,
                exact_content: true,
                extra_headers,
            },
        )
        .await?;
        let response = stream_success(response, timeout).await?;
        let content_type = response.header("content-type");
        if !is_media_type(content_type, "text/event-stream") {
            return Err(Error::NotEventStream {
                content_type: content_type.map(str::to_owned),
            });
        }
        let mut parser = EventStreamParser::with_max_event_bytes(source.max_event_bytes);
        if let Some(id) = &source.last_event_id {
            parser = parser.with_last_event_id(id.clone());
        }
        Ok(EventStream {
            body: response.body,
            parser,
            ready: VecDeque::new(),
            silence: source.silence(source.ping),
            source: source.clone(),
        })
    }

    /// The URL [`event_source`](Self::event_source) opens: the
    /// session's `eventSourceUrl` template with `source`'s `types`,
    /// `closeafter` and `ping` filled in.
    pub fn event_source_url(&self, source: &EventSource) -> Result<Url, Error> {
        self.session().event_source_endpoint(
            self.session_url(),
            &source.types_variable(),
            if source.close_after_state {
                "state"
            } else {
                "no"
            },
            u32::try_from(source.ping.as_secs()).unwrap_or(u32::MAX),
        )
    }
}

/// An open event source: the events of one response, read with
/// [`next`](Self::next). Dropping it closes the connection.
#[derive(Debug)]
pub struct EventStream<B> {
    body: B,
    parser: EventStreamParser,
    ready: VecDeque<ServerSentEvent>,
    /// How long the stream may be silent now.
    silence: Option<Duration>,
    source: EventSource,
}

impl<B: BodyStream> EventStream<B> {
    /// A stream over a response body that is already open: for a
    /// request made by other means than [`Client::event_source`].
    pub fn new(body: B, source: &EventSource) -> Self {
        let mut parser = EventStreamParser::with_max_event_bytes(source.max_event_bytes);
        if let Some(id) = &source.last_event_id {
            parser = parser.with_last_event_id(id.clone());
        }
        Self {
            body,
            parser,
            ready: VecDeque::new(),
            silence: source.silence(source.ping),
            source: source.clone(),
        }
    }

    /// The next event, or `None` when the server has ended the
    /// response (after each state event with
    /// [`close_after_state`](EventSource::close_after_state); otherwise
    /// when it shuts down or sheds connections).
    ///
    /// A ping is returned like any event, and sets how long the stream
    /// may then be silent: the interval the server says it pings at,
    /// plus the grace.
    ///
    /// # Errors
    ///
    /// [`Error::Transport`] of kind
    /// [`Timeout`](TransportErrorKind::Timeout) when nothing arrived
    /// for longer than the pings allow ([`Error::is_timeout`]), of kind
    /// [`TooLarge`](TransportErrorKind::TooLarge) for a line or an event
    /// over the limit, and whatever the transport reports when the
    /// connection breaks. The stream is finished after an error.
    pub async fn next(&mut self) -> Result<Option<EventSourceEvent>, Error> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                let event = EventSourceEvent::from_event(event);
                if let EventSourceEvent::Ping {
                    interval: Some(interval),
                } = &event
                {
                    self.silence = self.source.silence(if interval.is_zero() {
                        *interval
                    } else {
                        (*interval).max(MIN_ANNOUNCED_PING)
                    });
                }
                return Ok(Some(event));
            }
            let Some(chunk) = self.body.chunk(self.silence).await? else {
                return Ok(None);
            };
            self.ready.extend(self.parser.feed(&chunk)?);
        }
    }

    /// The stream's last event id, for the next stream's
    /// [`EventSource::last_event_id`]; `None` when the server sends no
    /// ids.
    pub fn last_event_id(&self) -> Option<&str> {
        self.parser.last_event_id()
    }

    /// The reconnection time the server last asked for (the `retry`
    /// field), if it did.
    pub fn retry(&self) -> Option<Duration> {
        self.parser.retry()
    }

    /// How long the stream may now be silent before
    /// [`next`](Self::next) fails; `None` for no limit.
    pub fn idle_timeout(&self) -> Option<Duration> {
        self.silence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event: &str, data: &str, id: Option<&str>) -> ServerSentEvent {
        ServerSentEvent {
            event: event.to_owned(),
            data: data.to_owned(),
            id: id.map(str::to_owned),
        }
    }

    fn all(stream: &[u8]) -> Vec<ServerSentEvent> {
        EventStreamParser::new().feed(stream).expect("parses")
    }

    /// The examples of the specification's section 9.2.6.
    #[test]
    fn the_specifications_examples_parse_as_it_says() {
        assert_eq!(
            all(b"data: YHOO\ndata: +2\ndata: 10\n\n"),
            [event("message", "YHOO\n+2\n10", None)]
        );
        // A comment, an event with an id, one that keeps it, and one
        // whose empty id resets it.
        assert_eq!(
            all(b": test stream\n\ndata: first event\nid: 1\n\ndata:second event\nid\n\ndata:  third event\n\n"),
            [
                event("message", "first event", Some("1")),
                event("message", "second event", None),
                event("message", " third event", None),
            ]
        );
        // An empty data field is an event; one alone before the end of
        // the stream is not dispatched.
        assert_eq!(
            all(b"data\n\ndata\ndata\n\ndata:"),
            [event("message", "", None), event("message", "\n", None)]
        );
        // Only one space after the colon is dropped.
        assert_eq!(
            all(b"data:test\n\ndata: test\n\n"),
            [
                event("message", "test", None),
                event("message", "test", None)
            ]
        );
    }

    #[test]
    fn every_line_ending_ends_a_line_wherever_the_bytes_are_cut() {
        let stream = b"event: state\rdata: a\r\ndata: b\n\r\nevent: ping\r\rdata: c\n\n";
        let expected = [event("state", "a\nb", None), event("message", "c", None)];
        assert_eq!(all(stream), expected);
        // The same stream a byte at a time: a CR and its LF arrive apart.
        for size in 1..stream.len() {
            let mut parser = EventStreamParser::new();
            let mut events = Vec::new();
            for piece in stream.chunks(size) {
                events.extend(parser.feed(piece).expect("parses"));
            }
            assert_eq!(events, expected, "in pieces of {size}");
        }
    }

    #[test]
    fn ids_and_retry_follow_the_specification() {
        let mut parser = EventStreamParser::new().with_last_event_id("earlier");
        // An event without an id reports the one the stream began with.
        let events = parser.feed(b"data: a\n\n").expect("parses");
        assert_eq!(events, [event("message", "a", Some("earlier"))]);
        // An id with a NUL is ignored; an id on an event without data
        // still becomes the last event id; retry takes digits only.
        let events = parser
            .feed(b"id: bad\0id\nretry: 2500\ndata: b\n\nid: 9\nretry: 1s\n\nretry\ndata: c\n\n")
            .expect("parses");
        assert_eq!(
            events,
            [
                event("message", "b", Some("earlier")),
                event("message", "c", Some("9"))
            ]
        );
        assert_eq!(parser.last_event_id(), Some("9"));
        assert_eq!(parser.retry(), Some(Duration::from_millis(2500)));
    }

    #[test]
    fn a_byte_order_mark_unknown_fields_and_bad_utf8_are_tolerated() {
        assert_eq!(
            all(b"\xEF\xBB\xBFdata: a\nfoo: bar\nEvent: no\ndata\xFF: x\n\n"),
            [event("message", "a", None)]
        );
        assert_eq!(
            all(b"event: state\ndata: caf\xC3\xA9 \xFF\n\n"),
            [event("state", "caf\u{e9} \u{fffd}", None)]
        );
        // An event type is reset by an event without data.
        assert_eq!(
            all(b"event: ping\n\ndata: plain\n\n"),
            [event("message", "plain", None)]
        );
    }

    #[test]
    fn lines_and_events_over_the_limit_are_refused() {
        let mut parser = EventStreamParser::with_max_event_bytes(16);
        assert!(parser.feed(b"data: 0123456789\n").expect("fits").is_empty());
        let error = parser
            .feed(b"data: 0123456789\n")
            .expect_err("too much data");
        assert_eq!(error.kind(), TransportErrorKind::TooLarge);

        let mut parser = EventStreamParser::with_max_event_bytes(16);
        let error = parser
            .feed(b": a comment that never ends")
            .expect_err("too long a line");
        assert_eq!(error.kind(), TransportErrorKind::TooLarge);
    }

    #[test]
    fn events_are_read_as_what_they_mean_in_jmap() {
        let state = r#"{"@type":"StateChange","changed":{"a1":{"Email":"s2"}}}"#;
        for name in ["state", "message"] {
            let EventSourceEvent::StateChange(change) =
                EventSourceEvent::from_event(event(name, state, None))
            else {
                panic!("{name} is a state change");
            };
            assert_eq!(change.state("a1", "Email"), Some("s2"));
        }
        assert_eq!(
            EventSourceEvent::from_event(event("ping", r#"{"interval":300}"#, None)),
            EventSourceEvent::Ping {
                interval: Some(Duration::from_secs(300))
            }
        );
        assert_eq!(
            EventSourceEvent::from_event(event("ping", "", None)),
            EventSourceEvent::Ping { interval: None }
        );
        for (name, data) in [
            ("state", "not JSON"),
            ("state", r#"{"@type":"PushVerification"}"#),
            ("vendor", state),
        ] {
            assert_eq!(
                EventSourceEvent::from_event(event(name, data, None)),
                EventSourceEvent::Other(event(name, data, None))
            );
        }
    }

    #[test]
    fn silence_is_measured_by_the_pings() {
        let source = EventSource::new();
        assert_eq!(
            source.silence(source.ping),
            Some(Duration::from_secs(45)),
            "30 seconds and the grace"
        );
        let quiet = EventSource::new().ping(Duration::ZERO);
        assert_eq!(quiet.silence(quiet.ping), None);
        let bounded = quiet.idle_timeout(Duration::from_secs(90));
        assert_eq!(bounded.silence(bounded.ping), Some(Duration::from_secs(90)));
        assert_eq!(EventSource::new().types_variable(), "*");
        assert_eq!(
            EventSource::new()
                .types(["Email", "Mailbox"])
                .types_variable(),
            "Email,Mailbox"
        );
    }
}
