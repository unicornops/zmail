//! JMAP push: the event-source binding (RFC 8620 section 7.3) with a
//! fallback to the WebSocket binding (RFC 8887).
//!
//! A push session holds one long-lived connection and reports every
//! `StateChange` that touches mail. It never syncs by itself: the caller
//! turns each [`PushEvent`] into an incremental sync (`Email/changes`), so
//! the local store stays the only thing the UI reads.

use std::{
    collections::HashMap,
    sync::Once,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use jmap_client::{DataType, PushObject, client::Client, client_ws::WebSocketMessage};
use reqwest::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use serde::Deserialize;
use url::Url;
use zmail_core::{ProtocolKind, SyncTarget};

use crate::connect_client;

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
/// Shortest stall timeout honoured when a server announces its own ping
/// interval.
const MIN_PING_INTERVAL: Duration = Duration::from_secs(5);
/// Connection timeout for opening the push stream.
const PUSH_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Largest single server-sent event accepted, mirroring `jmap-client`.
const MAX_EVENT_BYTES: usize = 1024 * 1024;
/// Data types the push subscription asks for.
const PUSH_TYPES: [DataType; 2] = [DataType::Email, DataType::Mailbox];
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
    mut on_event: F,
) -> Result<PushEnd>
where
    F: FnMut(PushEvent) + Send,
{
    if target.protocol != ProtocolKind::Jmap {
        bail!("JMAP adapter received a non-JMAP sync target");
    }

    let client = connect_client(&target.endpoint, &target.username, secret, target.auth).await?;
    let account_id = client.default_account_id().to_owned();
    let websocket_push = client
        .session()
        .websocket_capabilities()
        .is_some_and(|capabilities| capabilities.supports_push());

    if preference != PushPreference::WebSocket {
        match run_event_source(&client, &account_id, &mut on_event).await {
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

    run_websocket(&client, &account_id, &mut on_event).await
}

enum EventSourceError {
    /// The stream could not be opened at all.
    Open(anyhow::Error),
    /// The stream opened and then failed.
    Stream(anyhow::Error),
}

async fn run_event_source<F>(
    client: &Client,
    account_id: &str,
    on_event: &mut F,
) -> Result<PushEnd, EventSourceError>
where
    F: FnMut(PushEvent) + Send,
{
    let session = client.session();
    let url = event_source_url(
        client.session_url(),
        session.event_source_url(),
        PUSH_PING_INTERVAL,
    )
    .map_err(EventSourceError::Open)?;

    let mut headers = client.headers().clone();
    headers.remove(CONTENT_TYPE);
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let allowed_host = url.host_str().map(str::to_owned);
    let http = reqwest::Client::builder()
        .connect_timeout(PUSH_CONNECT_TIMEOUT)
        .default_headers(headers)
        // Credentials only ever go to the host the session named.
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > 5 || attempt.url().host_str() != allowed_host.as_deref() {
                attempt.stop()
            } else {
                attempt.follow()
            }
        }))
        .build()
        .context("failed to build the JMAP event-source client")
        .map_err(EventSourceError::Open)?;

    let response = http
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("failed to open the JMAP event source {url}"))
        .map_err(EventSourceError::Open)?;
    if !response.status().is_success() {
        return Err(EventSourceError::Open(anyhow!(
            "the JMAP event source answered HTTP {}",
            response.status()
        )));
    }
    let is_event_stream = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with("text/event-stream"));
    if !is_event_stream {
        return Err(EventSourceError::Open(anyhow!(
            "the JMAP event source did not answer with text/event-stream"
        )));
    }

    on_event(PushEvent::Connected(PushTransport::EventSource));
    let mut stall_timeout = PUSH_PING_INTERVAL + STALL_GRACE;
    let mut parser = SseParser::default();
    let mut body = response.bytes_stream();
    loop {
        let chunk = match tokio::time::timeout(stall_timeout, body.next()).await {
            Err(_elapsed) => {
                return Err(EventSourceError::Stream(anyhow!(
                    "the JMAP event source was silent for {}s",
                    stall_timeout.as_secs()
                )));
            }
            Ok(None) => return Ok(PushEnd::Closed(PushTransport::EventSource)),
            Ok(Some(Err(error))) => {
                return Err(EventSourceError::Stream(
                    anyhow!(error).context("the JMAP event source failed"),
                ));
            }
            Ok(Some(Ok(chunk))) => chunk,
        };
        let events = parser.push(&chunk).map_err(EventSourceError::Stream)?;
        for event in events {
            match event.event.as_str() {
                "ping" => {
                    // The server may pick its own interval and announces it
                    // in the ping data; the stall timeout follows it.
                    if let Some(interval) = ping_interval(&event.data) {
                        stall_timeout = interval.max(MIN_PING_INTERVAL) + STALL_GRACE;
                    }
                }
                // RFC 8620 names the event "state"; servers that omit the
                // event field send the default "message" type.
                "state" | "message" | "" => match StateChange::parse(&event.data) {
                    Ok(Some(change)) if change.touches_mail(account_id) => {
                        on_event(PushEvent::MailChanged);
                    }
                    Ok(_) => {}
                    // An unreadable change still means something changed.
                    Err(_) => on_event(PushEvent::MailChanged),
                },
                _ => {}
            }
        }
    }
}

async fn run_websocket<F>(client: &Client, account_id: &str, on_event: &mut F) -> Result<PushEnd>
where
    F: FnMut(PushEvent) + Send,
{
    ensure_rustls_provider();
    let mut stream = client
        .connect_ws()
        .await
        .context("failed to open the JMAP WebSocket")?;
    client
        .enable_push_ws(Some(PUSH_TYPES), Option::<String>::None)
        .await
        .context("failed to enable JMAP WebSocket push")?;
    // The server does not acknowledge WebSocketPushEnable, but it handles
    // messages in order, so the answer to a request sent after it shows the
    // subscription is in place before the caller is told it is connected.
    send_heartbeat(client).await?;
    let confirmed = tokio::time::timeout(PUSH_CONNECT_TIMEOUT, async {
        loop {
            match stream.next().await {
                Some(Ok(WebSocketMessage::Response(_))) => return Ok(()),
                Some(Ok(_)) | Some(Err(jmap_client::Error::Parse(_))) => {}
                Some(Err(error)) => {
                    return Err(anyhow!(error).context("the JMAP WebSocket failed"));
                }
                None => bail!("the JMAP WebSocket closed before push was enabled"),
            }
        }
    })
    .await;
    match confirmed {
        Ok(result) => result?,
        Err(_) => bail!("the JMAP WebSocket did not answer after enabling push"),
    }
    on_event(PushEvent::Connected(PushTransport::WebSocket));

    // `jmap-client` hides WebSocket pings and pongs, so liveness is checked
    // with a cheap request whose response must come back in time.
    let stall_timeout = PUSH_PING_INTERVAL + STALL_GRACE;
    let mut heartbeat = tokio::time::interval_at(
        tokio::time::Instant::now() + PUSH_PING_INTERVAL,
        PUSH_PING_INTERVAL,
    );
    let mut last_message = Instant::now();
    loop {
        tokio::select! {
            message = stream.next() => match message {
                None => return Ok(PushEnd::Closed(PushTransport::WebSocket)),
                Some(Ok(WebSocketMessage::PushNotification(PushObject::StateChange { changed }))) => {
                    last_message = Instant::now();
                    if StateChange::from_typed(changed).touches_mail(account_id) {
                        on_event(PushEvent::MailChanged);
                    }
                }
                Some(Ok(_)) => last_message = Instant::now(),
                // A push naming a type `jmap-client` does not know fails to
                // decode; treat it as a change rather than dropping it.
                Some(Err(jmap_client::Error::Parse(_))) => {
                    last_message = Instant::now();
                    on_event(PushEvent::MailChanged);
                }
                Some(Err(error)) => {
                    return Err(anyhow!(error).context("the JMAP WebSocket failed"));
                }
            },
            _ = heartbeat.tick() => {
                if last_message.elapsed() > stall_timeout {
                    bail!("the JMAP WebSocket was silent for {}s", stall_timeout.as_secs());
                }
                send_heartbeat(client).await?;
            }
        }
    }
}

/// Sends a cheap request (`Mailbox/get` with no ids) whose response proves
/// the WebSocket is alive and has processed everything sent before it.
async fn send_heartbeat(client: &Client) -> Result<()> {
    let mut request = client.build();
    request.get_mailbox().ids(Vec::<String>::new());
    client
        .send_ws(request)
        .await
        .context("failed to send the JMAP WebSocket heartbeat")?;
    Ok(())
}

fn ensure_rustls_provider() {
    static RUSTLS_PROVIDER: Once = Once::new();

    RUSTLS_PROVIDER.call_once(|| {
        let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// Expands the session's `eventSourceUrl` template (RFC 8620 section 7.3).
///
/// The template variables are `types`, `closeafter` and `ping`. The stream
/// stays open (`closeafter=no`) and asks for Email and Mailbox changes; a
/// relative template resolves against the session URL.
fn event_source_url(session_url: &str, template: &str, ping: Duration) -> Result<Url> {
    let types = PUSH_TYPES
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let expanded = template
        .replace("{types}", &types)
        .replace("{closeafter}", "no")
        .replace("{ping}", &ping.as_secs().to_string());
    let base = Url::parse(session_url)
        .with_context(|| format!("invalid JMAP session URL {session_url}"))?;
    let url = base
        .join(&expanded)
        .with_context(|| format!("invalid JMAP eventSourceUrl {template}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("the JMAP eventSourceUrl must be an HTTP or HTTPS URL");
    }
    Ok(url)
}

/// Reads the interval a ping event announces (`{"interval": 30}`).
fn ping_interval(data: &str) -> Option<Duration> {
    #[derive(Deserialize)]
    struct Ping {
        interval: u64,
    }
    serde_json::from_str::<Ping>(data)
        .ok()
        .map(|ping| Duration::from_secs(ping.interval))
}

/// A decoded `StateChange` push object (RFC 8620 section 7.1).
///
/// Type names stay strings so a server announcing a type this client does
/// not model still decodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StateChange {
    changed: HashMap<String, HashMap<String, String>>,
}

impl StateChange {
    /// Parses push data, returning `None` for other push object types.
    fn parse(data: &str) -> Result<Option<Self>> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(rename = "@type")]
            kind: String,
            #[serde(default)]
            changed: HashMap<String, HashMap<String, String>>,
        }
        let raw: Raw = serde_json::from_str(data).context("failed to decode JMAP push data")?;
        Ok((raw.kind == "StateChange").then_some(Self {
            changed: raw.changed,
        }))
    }

    fn from_typed<A, T>(changed: A) -> Self
    where
        A: IntoIterator<Item = (String, T)>,
        T: IntoIterator<Item = (DataType, String)>,
    {
        Self {
            changed: changed
                .into_iter()
                .map(|(account, types)| {
                    let types = types
                        .into_iter()
                        .map(|(kind, state)| (kind.to_string(), state))
                        .collect();
                    (account, types)
                })
                .collect(),
        }
    }

    /// Whether the change touches mail data of `account_id`.
    fn touches_mail(&self, account_id: &str) -> bool {
        self.changed
            .get(account_id)
            .is_some_and(|types| types.keys().any(|kind| MAIL_TYPES.contains(&kind.as_str())))
    }
}

/// One dispatched server-sent event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SseEvent {
    event: String,
    data: String,
}

/// Incremental `text/event-stream` parser (HTML living standard, section
/// 9.2.6), restricted to the fields JMAP uses.
#[derive(Debug, Default)]
struct SseParser {
    line: Vec<u8>,
    event: String,
    data: String,
}

impl SseParser {
    /// Feeds bytes and returns the events they complete.
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        let mut events = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() > MAX_EVENT_BYTES {
                    bail!("a JMAP event-source line exceeded {MAX_EVENT_BYTES} bytes");
                }
                continue;
            }
            let mut line = std::mem::take(&mut self.line);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.process_line(&line)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn process_line(&mut self, line: &[u8]) -> Result<Option<SseEvent>> {
        if line.is_empty() {
            // A blank line dispatches the event; one without data is
            // dropped, as the standard says.
            let event = std::mem::take(&mut self.event);
            let mut data = std::mem::take(&mut self.data);
            if data.is_empty() {
                return Ok(None);
            }
            data.pop();
            return Ok(Some(SseEvent { event, data }));
        }
        if line.first() == Some(&b':') {
            return Ok(None);
        }
        let line = String::from_utf8_lossy(line);
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line.as_ref(), ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.data.len() + value.len() > MAX_EVENT_BYTES {
                    bail!("a JMAP event-source event exceeded {MAX_EVENT_BYTES} bytes");
                }
                self.data.push_str(value);
                self.data.push('\n');
            }
            // `id` and `retry` only matter for replay, which the caller's
            // resync on connect replaces.
            _ => {}
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_dispatches_events_on_blank_lines() {
        let mut parser = SseParser::default();
        let events = parser
            .push(b"event: state\ndata: {\"a\":1}\n\n: comment\nevent: ping\ndata: x\n\n")
            .expect("valid stream");
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: "state".to_owned(),
                    data: "{\"a\":1}".to_owned()
                },
                SseEvent {
                    event: "ping".to_owned(),
                    data: "x".to_owned()
                },
            ]
        );
    }

    #[test]
    fn sse_parser_handles_crlf_split_chunks_and_multiline_data() {
        let mut parser = SseParser::default();
        assert!(parser.push(b"event: st").expect("partial line").is_empty());
        assert!(
            parser
                .push(b"ate\r\ndata: one\r\nda")
                .expect("partial event")
                .is_empty()
        );
        let events = parser.push(b"ta:two\r\n\r\n").expect("complete event");
        assert_eq!(
            events,
            vec![SseEvent {
                event: "state".to_owned(),
                data: "one\ntwo".to_owned()
            }]
        );
    }

    #[test]
    fn sse_parser_drops_events_without_data_and_resets_the_type() {
        let mut parser = SseParser::default();
        let events = parser
            .push(b"event: ping\n\ndata: plain\n\n")
            .expect("valid stream");
        assert_eq!(
            events,
            vec![SseEvent {
                event: String::new(),
                data: "plain".to_owned()
            }]
        );
    }

    #[test]
    fn sse_parser_rejects_oversized_lines() {
        let mut parser = SseParser::default();
        let oversized = vec![b'a'; MAX_EVENT_BYTES + 1];
        assert!(parser.push(&oversized).is_err());
    }

    #[test]
    fn state_changes_touch_mail_only_for_mail_types_of_the_account() {
        let change = StateChange::parse(
            r#"{"@type":"StateChange","changed":{"a1":{"Email":"s2","CalendarEvent":"c1"},"a2":{"Identity":"i1"}}}"#,
        )
        .expect("valid push data")
        .expect("a StateChange");
        assert!(change.touches_mail("a1"));
        assert!(!change.touches_mail("a2"), "Identity is not mail data");
        assert!(!change.touches_mail("a3"), "other accounts are ignored");

        let delivery = StateChange::parse(
            r#"{"@type":"StateChange","changed":{"a1":{"EmailDelivery":"d1"}}}"#,
        )
        .expect("valid push data")
        .expect("a StateChange");
        assert!(delivery.touches_mail("a1"));
    }

    #[test]
    fn other_push_objects_are_not_state_changes() {
        assert_eq!(
            StateChange::parse(r#"{"@type":"PushVerification","pushSubscriptionId":"p"}"#)
                .expect("valid push data"),
            None
        );
        assert!(StateChange::parse("not json").is_err());
    }

    #[test]
    fn typed_state_changes_convert_to_type_names() {
        let mut types = HashMap::new();
        types.insert(DataType::Mailbox, "m2".to_owned());
        let mut changed = HashMap::new();
        changed.insert("a1".to_owned(), types);
        assert!(StateChange::from_typed(changed).touches_mail("a1"));
    }

    #[test]
    fn ping_events_announce_their_interval() {
        assert_eq!(
            ping_interval(r#"{"interval":45}"#),
            Some(Duration::from_secs(45))
        );
        assert_eq!(ping_interval("{}"), None);
    }

    #[test]
    fn event_source_templates_expand_and_resolve() {
        let url = event_source_url(
            "http://127.0.0.1:4080/.well-known/jmap",
            "http://127.0.0.1:4080/eventSource?types={types}&closeafter={closeafter}&ping={ping}",
            Duration::from_secs(30),
        )
        .expect("valid template");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:4080/eventSource?types=Email,Mailbox&closeafter=no&ping=30"
        );

        let relative = event_source_url(
            "https://mail.example.test/.well-known/jmap",
            "/jmap/eventsource/?types={types}&closeafter={closeafter}&ping={ping}",
            Duration::from_secs(10),
        )
        .expect("relative template");
        assert_eq!(
            relative.as_str(),
            "https://mail.example.test/jmap/eventsource/?types=Email,Mailbox&closeafter=no&ping=10"
        );

        assert!(
            event_source_url(
                "https://mail.example.test/.well-known/jmap",
                "ftp://mail.example.test/events",
                Duration::from_secs(10),
            )
            .is_err()
        );
    }
}
