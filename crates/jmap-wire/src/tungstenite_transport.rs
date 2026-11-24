//! The default [`WebSocketTransport`], on `tokio-tungstenite`.

use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{
        self, Message,
        client::IntoClientRequest as _,
        http::{HeaderName, HeaderValue},
        protocol::WebSocketConfig,
    },
};

use crate::{
    error::{TransportError, TransportErrorKind},
    transport::without_query,
    websocket::{WebSocket, WebSocketHandshake, WebSocketTransport},
};

/// A [`WebSocketTransport`] on `tokio-tungstenite`: TLS by rustls
/// against the system's root certificates, no redirects, and no headers
/// but the handshake's own.
///
/// rustls needs a process-wide crypto provider when more than one is
/// compiled in (`ring` and `aws-lc-rs`, as happens when other crates of
/// a build turn both on): install one with
/// `rustls::crypto::CryptoProvider::install_default` before the first
/// `wss` connection, or give the transport a TLS configuration of your
/// own ([`with_connector`](Self::with_connector)).
#[derive(Clone, Default)]
pub struct TungsteniteTransport {
    connector: Option<Connector>,
}

impl std::fmt::Debug for TungsteniteTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TungsteniteTransport")
            .field(
                "connector",
                &if self.connector.is_some() {
                    "custom"
                } else {
                    "default"
                },
            )
            .finish()
    }
}

impl TungsteniteTransport {
    /// A transport that verifies servers against the system's root
    /// certificates.
    pub fn new() -> Self {
        Self::default()
    }

    /// A transport with a TLS configuration of your own (a
    /// `Connector::Rustls` with your roots, say).
    ///
    /// `Connector::Plain` makes no TLS connection at all: a `wss` URL
    /// then fails. Do not build a configuration that skips certificate
    /// verification; the handshake carries the credentials.
    pub fn with_connector(connector: Connector) -> Self {
        Self {
            connector: Some(connector),
        }
    }
}

impl WebSocketTransport for TungsteniteTransport {
    type Socket = TungsteniteSocket;

    async fn connect(
        &self,
        handshake: WebSocketHandshake,
    ) -> Result<TungsteniteSocket, TransportError> {
        let shown = without_query(&handshake.url);
        let bad_header = |name: &str| {
            TransportError::new(
                TransportErrorKind::Other,
                format!("the {name} header cannot be sent"),
            )
        };
        let mut request = handshake
            .url
            .as_str()
            .into_client_request()
            .map_err(|error| describe(error, &shown, "could not be opened"))?;
        for (name, value) in &handshake.headers {
            request.headers_mut().insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| bad_header(name))?,
                HeaderValue::from_str(value).map_err(|_| bad_header(name))?,
            );
        }
        // tungstenite fails the handshake unless the server's answer
        // names this subprotocol.
        request.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_str(handshake.subprotocol)
                .map_err(|_| bad_header("sec-websocket-protocol"))?,
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(handshake.max_message_bytes))
            .max_frame_size(Some(handshake.max_frame_bytes));
        let connecting = tokio_tungstenite::connect_async_tls_with_config(
            request,
            Some(config),
            false,
            self.connector.clone(),
        );
        let (stream, _) = tokio::time::timeout(handshake.timeout, connecting)
            .await
            .map_err(|_elapsed| {
                TransportError::new(
                    TransportErrorKind::Timeout,
                    format!("the WebSocket connection to {shown} timed out"),
                )
            })?
            .map_err(|error| describe(error, &shown, "could not be opened"))?;
        Ok(TungsteniteSocket { stream, url: shown })
    }
}

/// A connection [`TungsteniteTransport`] opened.
#[derive(Debug)]
pub struct TungsteniteSocket {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    /// The URL without its query string, for error messages.
    url: String,
}

impl WebSocket for TungsteniteSocket {
    async fn send(&mut self, text: String) -> Result<(), TransportError> {
        self.stream
            .send(Message::text(text))
            .await
            .map_err(|error| describe(error, &self.url, "failed"))
    }

    async fn receive(&mut self, idle: Option<Duration>) -> Result<Option<String>, TransportError> {
        let next = async {
            loop {
                // `next` on a stream loses nothing when it is dropped.
                match self.stream.next().await {
                    None | Some(Ok(Message::Close(_))) => return Ok(None),
                    Some(Ok(Message::Text(text))) => return Ok(Some(text.as_str().to_owned())),
                    // Pings are answered by the library as it reads on;
                    // binary messages are not JMAP's.
                    Some(Ok(_)) => {}
                    Some(Err(
                        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed,
                    )) => return Ok(None),
                    Some(Err(error)) => return Err(error),
                }
            }
        };
        let received = match idle {
            Some(idle) => tokio::time::timeout(idle, next).await.map_err(|_elapsed| {
                TransportError::new(
                    TransportErrorKind::Timeout,
                    format!(
                        "the WebSocket connection to {} was silent for {} seconds",
                        self.url,
                        idle.as_secs()
                    ),
                )
            })?,
            None => next.await,
        };
        received.map_err(|error| describe(error, &self.url, "failed"))
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        match self.stream.close(None).await {
            Ok(())
            | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                Ok(())
            }
            Err(error) => Err(describe(error, &self.url, "failed")),
        }
    }
}

/// A `tungstenite` error as a transport error: its kind, the status of
/// a handshake the server refused, and a message with the URL without
/// its query string.
fn describe(error: tungstenite::Error, url: &str, what: &str) -> TransportError {
    let (kind, status) = match &error {
        tungstenite::Error::Http(response) => {
            (TransportErrorKind::Other, Some(response.status().as_u16()))
        }
        tungstenite::Error::Io(_) | tungstenite::Error::Tls(_) | tungstenite::Error::Url(_) => {
            (TransportErrorKind::Connect, None)
        }
        tungstenite::Error::Capacity(_) => (TransportErrorKind::TooLarge, None),
        _ => (TransportErrorKind::Other, None),
    };
    let described = TransportError::new(
        kind,
        format!("the WebSocket connection to {url} {what}: {error}"),
    );
    match status {
        // The response is dropped with the error: its body is not kept.
        Some(status) => described.with_status(status),
        None => described.with_source(error),
    }
}
