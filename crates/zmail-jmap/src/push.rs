//! JMAP push: the event-source binding (RFC 8620 section 7.3) with a
//! fallback to the WebSocket binding (RFC 8887).
//!
//! A push session holds one long-lived connection and reports every
//! `StateChange` that touches mail. It never syncs by itself: the caller
//! turns each [`PushEvent`] into an incremental sync (`Email/changes`), so
//! the local store stays the only thing the UI reads.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use jmap_wire::{
    Get, StateChange, TungsteniteTransport,
    event_source::{EventSource, EventSourceEvent},
    mail::Mailbox,
    websocket::{PushEnable, WebSocketConnection, WebSocketMessage},
};
use tokio_tungstenite::Connector;
use zmail_core::{ProtocolKind, SyncTarget};

use crate::{Connection, connect_client, wording};

/// Keepalive interval requested from the server.
///
/// The server sends a ping at least this often on an otherwise idle
/// connection, so silence for longer than the interval plus
/// [`STALL_GRACE`] means the connection is dead (for example after a
/// network change) and is dropped instead of waiting forever.
pub const PUSH_PING_INTERVAL: Duration = Duration::from_secs(30);
/// Slack on top of the ping interval before a silent connection counts as
/// stalled.
const STALL_GRACE: Duration = Duration::from_secs(15);
/// How often a WebSocket setup request is repeated before giving up, and
/// how long each attempt waits for the answer.
const WS_READY_ATTEMPTS: usize = 5;
const WS_READY_WAIT: Duration = Duration::from_secs(2);
/// How long connecting the push stream may take, and how long the
/// WebSocket may take to answer the request that follows enabling push.
const PUSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The intervals and waits of a push session. They are constants in the
/// app ([`PushTimings::DEFAULT`]); the tests shorten them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PushTimings {
    /// See [`PUSH_PING_INTERVAL`].
    pub(crate) ping_interval: Duration,
    /// See [`STALL_GRACE`].
    pub(crate) stall_grace: Duration,
    /// See [`WS_READY_ATTEMPTS`].
    pub(crate) ws_ready_attempts: usize,
    /// See [`WS_READY_WAIT`].
    pub(crate) ws_ready_wait: Duration,
    /// See [`PUSH_CONNECT_TIMEOUT`].
    pub(crate) connect_timeout: Duration,
}

impl PushTimings {
    pub(crate) const DEFAULT: Self = Self {
        ping_interval: PUSH_PING_INTERVAL,
        stall_grace: STALL_GRACE,
        ws_ready_attempts: WS_READY_ATTEMPTS,
        ws_ready_wait: WS_READY_WAIT,
        connect_timeout: PUSH_CONNECT_TIMEOUT,
    };
}

/// Data types the push subscription asks for.
const PUSH_TYPES: [&str; 2] = ["Email", "Mailbox"];
/// `StateChange` type names that mean the mail mirror may be stale.
const MAIL_TYPES: [&str; 4] = ["Email", "EmailDelivery", "Mailbox", "Thread"];

/// Transport a push session runs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushTransport {
    /// RFC 8620 section 7.3 server-sent events.
    EventSource,
    /// RFC 8887 WebSocket push (`WebSocketPushEnable`).
    WebSocket,
}

/// Which transport a push session tries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPreference {
    /// Event source first, WebSocket when the event source cannot be opened
    /// and the server offers WebSocket push.
    Auto,
    /// Only the event source.
    EventSource,
    /// Only WebSocket push.
    WebSocket,
}

/// Progress reported by a push session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushEvent {
    /// The push stream is open. Changes made while no stream was open are
    /// not replayed, so the caller resyncs once on every connect.
    Connected(PushTransport),
    /// The server reported a change to mail data for the account.
    MailChanged,
}

/// How a push session ended without a transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushEnd {
    /// The server closed an open stream; the caller reconnects.
    Closed(PushTransport),
    /// The server offers no usable push transport; the caller polls.
    Unsupported,
}

/// Holds one push connection for the account and reports mail changes
/// until the connection ends.
///
/// Returns `Ok(PushEnd::Closed)` when the server ends the stream cleanly,
/// `Ok(PushEnd::Unsupported)` when no push transport can be opened, and
/// `Err` on transport failures, including a connection that stays silent
/// past its keepalive interval, so the caller can back off and reconnect.
pub async fn watch_mail_changes<F>(
    target: &SyncTarget,
    secret: &str,
    preference: PushPreference,
    on_event: F,
) -> Result<PushEnd>
where
    F: FnMut(PushEvent) + Send,
{
    watch_with(target, secret, preference, PushTimings::DEFAULT, on_event).await
}

/// [`watch_mail_changes`] with the timings given.
pub(crate) async fn watch_with<F>(
    target: &SyncTarget,
    secret: &str,
    preference: PushPreference,
    timings: PushTimings,
    mut on_event: F,
) -> Result<PushEnd>
where
    F: FnMut(PushEvent) + Send,
{
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }

    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    let websocket_push = client
        .wire_session()
        .websocket()
        .is_some_and(|websocket| websocket.supports_push);

    if preference != PushPreference::WebSocket {
        match run_event_source(&client, timings, &mut on_event).await {
            Ok(end) => return Ok(end),
            // Only a stream that could not be opened falls back; a stream
            // that opened and then failed is a transport error to retry.
            Err(EventSourceError::Open(error)) => {
                if preference == PushPreference::EventSource || !websocket_push {
                    return if preference == PushPreference::Auto {
                        Ok(PushEnd::Unsupported)
                    } else {
                        Err(error)
                    };
                }
            }
            Err(EventSourceError::Stream(error)) => return Err(error),
        }
    } else if !websocket_push {
        bail!("the JMAP server does not offer WebSocket push");
    }

    run_websocket(&client, timings, &mut on_event).await
}

enum EventSourceError {
    /// The stream could not be opened at all.
    Open(anyhow::Error),
    /// The stream opened and then failed.
    Stream(anyhow::Error),
}

/// A `jmap-wire` failure of a push connection in words: a transport
/// failure by its own message, anything else as the rest of Zmail words
/// it.
fn push_error(error: jmap_wire::Error) -> anyhow::Error {
    match error {
        jmap_wire::Error::Transport(error) => anyhow!("{error}"),
        other => wording::reported(other),
    }
}

/// Why the event source did not open, in the words it always had where
/// the server answered, and in `jmap-wire`'s where no answer came.
fn open_error(error: jmap_wire::Error) -> anyhow::Error {
    if let Some(status) = error.http_status() {
        return match reqwest::StatusCode::from_u16(status) {
            Ok(status) => anyhow!("the JMAP event source answered HTTP {status}"),
            Err(_) => anyhow!("the JMAP event source answered HTTP {status}"),
        };
    }
    match error {
        // Worded by `jmap-wire` as it was here.
        error @ jmap_wire::Error::NotEventStream { .. } => anyhow!("{error}"),
        other => push_error(other).context("failed to open the JMAP event source"),
    }
}

async fn run_event_source<F>(
    client: &Connection,
    timings: PushTimings,
    on_event: &mut F,
) -> Result<PushEnd, EventSourceError>
where
    F: FnMut(PushEvent) + Send,
{
    // The stream stays open (`closeafter=no`) and asks for Email and
    // Mailbox changes. A server that says nothing at all, not even the
    // response's headers, for as long as a stream may be silent after
    // connecting is taken for one whose event source cannot be opened.
    let source = EventSource::new()
        .types(PUSH_TYPES)
        .ping(timings.ping_interval)
        .ping_grace(timings.stall_grace)
        .open_timeout(timings.connect_timeout + timings.ping_interval + timings.stall_grace);
    // Credentials only ever go to the host the session named for it.
    let mut events = client
        .event_source_client()
        .map_err(|error| EventSourceError::Open(open_error(error)))?
        .event_source(&source)
        .await
        .map_err(|error| EventSourceError::Open(open_error(error)))?;

    on_event(PushEvent::Connected(PushTransport::EventSource));
    let account_id = client.account_id();
    loop {
        // The server may pick its own ping interval and announces it in
        // each ping; how long the stream may be silent follows it.
        let stall_timeout = events.idle_timeout();
        match events.next().await {
            Ok(None) => return Ok(PushEnd::Closed(PushTransport::EventSource)),
            Ok(Some(EventSourceEvent::StateChange(change))) => {
                if touches_mail(&change, account_id) {
                    on_event(PushEvent::MailChanged);
                }
            }
            // RFC 8620 names the event "state"; servers that omit the
            // event field send the default "message" type. One that
            // cannot be read still means something changed; another
            // kind of push object does not.
            Ok(Some(EventSourceEvent::Other(event)))
                if matches!(event.event.as_str(), "state" | "message")
                    && StateChange::parse(&event.data).is_err() =>
            {
                on_event(PushEvent::MailChanged);
            }
            Ok(Some(_)) => {}
            Err(error) if error.is_timeout() => {
                return Err(EventSourceError::Stream(anyhow!(
                    "the JMAP event source was silent for {}s",
                    stall_timeout.unwrap_or_default().as_secs()
                )));
            }
            Err(error) => {
                return Err(EventSourceError::Stream(
                    push_error(error).context("the JMAP event source failed"),
                ));
            }
        }
    }
}

type Socket = WebSocketConnection<jmap_wire::TungsteniteSocket>;

async fn run_websocket<F>(
    client: &Connection,
    timings: PushTimings,
    on_event: &mut F,
) -> Result<PushEnd>
where
    F: FnMut(PushEvent) + Send,
{
    let account_id = client.account_id();
    let mut socket = client
        .wire()
        .connect_websocket(&websocket_transport()?)
        .await
        .map_err(|error| push_error(error).context("failed to open the JMAP WebSocket"))?;
    // Apache James 3.9 silently drops frames that arrive before its socket
    // handler starts reading, which happens right after the upgrade when
    // the server is busy (a lost WebSocketPushEnable leaves an open
    // connection that never pushes). A request is therefore repeated until
    // the server answers, and push is only enabled once it is reading.
    let mut reading = false;
    for _ in 0..timings.ws_ready_attempts {
        send_heartbeat(&mut socket, account_id).await?;
        if await_response(&mut socket, timings.ws_ready_wait).await? {
            reading = true;
            break;
        }
    }
    if !reading {
        bail!("the JMAP WebSocket never answered a request");
    }
    socket
        .enable_push(&PushEnable::new().data_types(PUSH_TYPES))
        .await
        .map_err(|error| push_error(error).context("failed to enable JMAP WebSocket push"))?;
    // The server does not acknowledge WebSocketPushEnable, but it handles
    // messages in order, so the answer to a request sent after it shows the
    // subscription is in place before the caller is told it is connected.
    send_heartbeat(&mut socket, account_id).await?;
    if !await_response(&mut socket, timings.connect_timeout).await? {
        bail!("the JMAP WebSocket did not answer after enabling push");
    }
    on_event(PushEvent::Connected(PushTransport::WebSocket));

    // Liveness is checked with a cheap request whose response must come
    // back in time: a WebSocket ping would show the connection is up,
    // not that the server still answers on it.
    let stall_timeout = timings.ping_interval + timings.stall_grace;
    let mut next_heartbeat = Instant::now() + timings.ping_interval;
    let mut last_message = Instant::now();
    loop {
        let until_heartbeat = next_heartbeat.saturating_duration_since(Instant::now());
        match socket.next(Some(until_heartbeat)).await {
            Ok(None) => return Ok(PushEnd::Closed(PushTransport::WebSocket)),
            Ok(Some(WebSocketMessage::StateChange(change))) => {
                last_message = Instant::now();
                if touches_mail(&change, account_id) {
                    on_event(PushEvent::MailChanged);
                }
            }
            Ok(Some(WebSocketMessage::RequestError { problem, .. })) => {
                return Err(request_error(problem));
            }
            // A message that cannot be read is treated as a change
            // rather than dropped.
            Ok(Some(WebSocketMessage::Unknown(_))) => {
                last_message = Instant::now();
                on_event(PushEvent::MailChanged);
            }
            Ok(Some(_)) => last_message = Instant::now(),
            // Time for a heartbeat.
            Err(error) if error.is_timeout() => {
                if last_message.elapsed() > stall_timeout {
                    bail!(
                        "the JMAP WebSocket was silent for {}s",
                        stall_timeout.as_secs()
                    );
                }
                send_heartbeat(&mut socket, account_id).await?;
                next_heartbeat += timings.ping_interval;
            }
            Err(error) => return Err(push_error(error).context("the JMAP WebSocket failed")),
        }
    }
}

/// A request-level error the server sent over the WebSocket (RFC 8887
/// section 4.3.4), which ends the session as a failure to retry.
fn request_error(problem: jmap_wire::ProblemDetails) -> anyhow::Error {
    wording::reported(jmap_wire::Error::Problem(Box::new(problem)))
        .context("the JMAP WebSocket failed")
}

/// Waits up to `wait` for the next method response on the WebSocket,
/// returning `false` when none arrives in time. Pushes received meanwhile
/// are dropped; the caller resyncs on connect anyway.
async fn await_response(socket: &mut Socket, wait: Duration) -> Result<bool> {
    let deadline = Instant::now() + wait;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        match socket.next(Some(remaining)).await {
            Ok(Some(WebSocketMessage::Response { .. })) => return Ok(true),
            Ok(Some(WebSocketMessage::RequestError { problem, .. })) => {
                return Err(request_error(problem));
            }
            Ok(Some(_)) => {}
            Ok(None) => bail!("the JMAP WebSocket closed during setup"),
            Err(error) if error.is_timeout() => return Ok(false),
            Err(error) => return Err(push_error(error).context("the JMAP WebSocket failed")),
        }
    }
}

/// Sends a cheap request (`Mailbox/get` with no ids) whose response proves
/// the WebSocket is alive and has processed everything sent before it.
async fn send_heartbeat(socket: &mut Socket, account_id: &str) -> Result<()> {
    let mut request = jmap_wire::Request::new();
    request
        .call(Get::<Mailbox>::new(account_id).ids(Vec::<String>::new()))
        .map_err(|error| anyhow!("failed to build the JMAP WebSocket heartbeat: {error}"))?;
    socket.send(&request).await.map_err(|error| {
        push_error(error).context("failed to send the JMAP WebSocket heartbeat")
    })?;
    Ok(())
}

/// The WebSocket transport, on the shared configuration of every mail
/// connection (ADR 0009) rather than tokio-tungstenite's own roots.
fn websocket_transport() -> Result<TungsteniteTransport> {
    let tls = zmail_security::tls::client_config()
        .map_err(|error| anyhow!("failed to set up the JMAP WebSocket: {error}"))?;
    Ok(TungsteniteTransport::with_connector(Connector::Rustls(tls)))
}

/// Whether a pushed change touches mail data of `account_id`.
fn touches_mail(change: &StateChange, account_id: &str) -> bool {
    change.touches(account_id, MAIL_TYPES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_changes_touch_mail_only_for_mail_types_of_the_account() {
        let change = StateChange::parse(
            r#"{"@type":"StateChange","changed":{"a1":{"Email":"s2","CalendarEvent":"c1"},"a2":{"Identity":"i1"}}}"#,
        )
        .expect("valid push data")
        .expect("a StateChange");
        assert!(touches_mail(&change, "a1"));
        assert!(!touches_mail(&change, "a2"), "Identity is not mail data");
        assert!(!touches_mail(&change, "a3"), "other accounts are ignored");

        for kind in MAIL_TYPES {
            let change = StateChange::parse(&format!(
                r#"{{"@type":"StateChange","changed":{{"a1":{{"{kind}":"d1"}}}}}}"#
            ))
            .expect("valid push data")
            .expect("a StateChange");
            assert!(touches_mail(&change, "a1"), "{kind}");
        }
    }

    #[test]
    fn a_refused_event_source_is_worded_by_its_status() {
        assert_eq!(
            open_error(jmap_wire::Error::Http { status: 404 }).to_string(),
            "the JMAP event source answered HTTP 404 Not Found"
        );
        assert_eq!(
            open_error(jmap_wire::Error::NotEventStream { content_type: None }).to_string(),
            "the JMAP event source did not answer with text/event-stream"
        );
    }
}
