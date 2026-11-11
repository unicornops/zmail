//! A progress timeout for IMAP connections.
//!
//! After connecting, every IMAP command waits on the server: the greeting,
//! the sign-in, a `FETCH` of a large message on a slow link, a `SEARCH` the
//! server takes a while over. A wall-clock limit per command would either cut
//! off a big download that is still arriving or leave a dead connection
//! hanging for as long as the biggest download could take. [`StallTimeout`]
//! instead fails a read or write that makes no progress at all for
//! [`STALL_TIMEOUT`]: any byte arriving (or leaving) starts the wait again,
//! so a slow but moving transfer always finishes, and a half-open connection
//! (a laptop that slept, a NAT that forgot the flow) fails in minutes rather
//! than hanging the account's sync, hydration or search for good.
//!
//! The watchers turn it off while they wait in IDLE, where minutes of
//! silence are normal; the IDLE renewal bounds that wait instead.

use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::{Instant, Sleep, sleep},
};

/// How long a read or write may make no progress before the connection
/// counts as dead.
///
/// Generous enough for commands a server works on before answering (a
/// `SEARCH` over a large mailbox, an `EXPUNGE`), short enough that a stalled
/// connection fails within a couple of minutes.
pub(crate) const STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Wraps a stream so a read or write that makes no progress for its timeout
/// fails with [`io::ErrorKind::TimedOut`].
#[derive(Debug)]
pub(crate) struct StallTimeout<S> {
    inner: S,
    timeout: Option<Duration>,
    read: Deadline,
    write: Deadline,
}

impl<S> StallTimeout<S> {
    /// Wraps `inner` with `timeout` (`None` waits for ever).
    pub(crate) fn new(inner: S, timeout: Option<Duration>) -> Self {
        Self {
            inner,
            timeout,
            read: Deadline::new(),
            write: Deadline::new(),
        }
    }

    /// The current timeout.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Changes the timeout; a wait already under way starts again.
    pub(crate) fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
        self.read.armed = false;
        self.write.armed = false;
    }
}

/// The wait for one direction of the stream. The timer is armed when a read
/// (or write) first has to wait, and disarmed whenever one completes.
#[derive(Debug)]
struct Deadline {
    sleep: Pin<Box<Sleep>>,
    armed: bool,
}

impl Deadline {
    fn new() -> Self {
        Self {
            sleep: Box::pin(sleep(Duration::ZERO)),
            armed: false,
        }
    }

    /// Called after the inner stream returned `Pending`: ready with the
    /// error once the stream has waited `timeout` without progress.
    fn poll_stalled(
        &mut self,
        cx: &mut Context<'_>,
        timeout: Option<Duration>,
        direction: &str,
    ) -> Poll<io::Error> {
        let Some(timeout) = timeout else {
            self.armed = false;
            return Poll::Pending;
        };
        if !self.armed {
            self.sleep.as_mut().reset(Instant::now() + timeout);
            self.armed = true;
        }
        match self.sleep.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.armed = false;
                Poll::Ready(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "the IMAP server stopped responding: nothing {direction} for {} seconds",
                        timeout.as_secs()
                    ),
                ))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for StallTimeout<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                this.read.armed = false;
                Poll::Ready(result)
            }
            Poll::Pending => this
                .read
                .poll_stalled(cx, this.timeout, "received")
                .map(Err),
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for StallTimeout<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Ready(result) => {
                this.write.armed = false;
                Poll::Ready(result)
            }
            Poll::Pending => this.write.poll_stalled(cx, this.timeout, "sent").map(Err),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Ready(result) => {
                this.write.armed = false;
                Poll::Ready(result)
            }
            Poll::Pending => this.write.poll_stalled(cx, this.timeout, "sent").map(Err),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_shutdown(cx) {
            Poll::Ready(result) => {
                this.write.armed = false;
                Poll::Ready(result)
            }
            Poll::Pending => this.write.poll_stalled(cx, this.timeout, "sent").map(Err),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    use super::StallTimeout;

    const SHORT: Duration = Duration::from_millis(200);

    #[tokio::test]
    async fn a_read_with_nothing_arriving_times_out() {
        let (client, _server) = duplex(64);
        let mut stream = StallTimeout::new(client, Some(SHORT));
        let started = std::time::Instant::now();
        let error = stream
            .read(&mut [0; 8])
            .await
            .expect_err("a silent peer must time out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= SHORT);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_slow_transfer_that_keeps_moving_is_never_cut_off() {
        let (client, mut server) = duplex(64);
        let mut stream = StallTimeout::new(client, Some(SHORT));
        // Ten bytes, one every 60 ms: four times the timeout in all, but
        // never more than a third of it without progress.
        let sender = tokio::spawn(async move {
            for byte in 0..10u8 {
                tokio::time::sleep(Duration::from_millis(60)).await;
                server.write_all(&[byte]).await.expect("write");
            }
            server
        });
        let mut received = Vec::new();
        while received.len() < 10 {
            let mut buf = [0; 8];
            let read = stream.read(&mut buf).await.expect("a moving read succeeds");
            received.extend_from_slice(&buf[..read]);
        }
        assert_eq!(received, (0..10).collect::<Vec<u8>>());
        drop(sender.await);
    }

    #[tokio::test]
    async fn without_a_timeout_a_silent_read_keeps_waiting() {
        let (client, _server) = duplex(64);
        let mut stream = StallTimeout::new(client, Some(SHORT));
        stream.set_timeout(None);
        let waited = tokio::time::timeout(SHORT * 3, stream.read(&mut [0; 8])).await;
        assert!(waited.is_err(), "the read must still be waiting");
    }

    #[tokio::test]
    async fn a_write_the_peer_never_drains_times_out() {
        // The pipe holds 16 bytes and nobody reads the other end.
        let (client, _server) = duplex(16);
        let mut stream = StallTimeout::new(client, Some(SHORT));
        let error = stream
            .write_all(&[0; 64])
            .await
            .expect_err("a full pipe must time out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }
}
