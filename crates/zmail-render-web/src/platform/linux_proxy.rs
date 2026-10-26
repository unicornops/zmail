//! The engine's network wall: a proxy that goes nowhere.
//!
//! WebKitGTK's user content filter blocks every load that is not Zmail's
//! scheme (`crate::rules`), and the page's Content Security Policy does the
//! same. Behind both, the engine's only network proxy is this listener on
//! the loopback interface: it accepts a connection, notes it and closes it.
//! A request that got past the filter therefore never reaches a name
//! server or another host. Zmail's own remote image fetches
//! (`crate::remote`) do not go through the engine, so they are unaffected.
//!
//! Safe code only.

use std::{
    io::{BufRead as _, BufReader, Read as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use crate::debug_log;

/// The black-hole proxy.
pub(crate) struct BlackHole {
    port: u16,
    connections: Arc<AtomicUsize>,
}

impl BlackHole {
    /// Starts listening on a free loopback port.
    pub(crate) fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let connections = Arc::new(AtomicUsize::new(0));
        let counted = connections.clone();
        thread::Builder::new()
            .name("zmail-web-proxy".to_owned())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let line = first_line(&stream);
                    eprintln!(
                        "zmail: web renderer: the engine tried to reach the network, \
                         and was stopped by the proxy wall"
                    );
                    debug_log(format_args!("proxy wall: {line}"));
                }
            })?;
        Ok(Self { port, connections })
    }

    /// The proxy URI for the engine.
    pub(crate) fn uri(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// How many connections reached the wall.
    pub(crate) fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// The first line of what a connection sent (its request line, at most
/// 512 bytes); the connection is dropped after.
fn first_line(stream: &TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    let mut line = String::new();
    let _ = BufReader::new(stream.take(512)).read_line(&mut line);
    line.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn connections_are_counted_and_closed() {
        let wall = BlackHole::start().expect("listening");
        assert!(wall.uri().starts_with("http://127.0.0.1:"));
        assert_eq!(wall.connections(), 0);
        let mut stream = TcpStream::connect(("127.0.0.1", wall.port)).expect("connected");
        stream
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n")
            .expect("written");
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut reply = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stream, &mut reply);
        assert!(reply.is_empty(), "the wall answered: {reply:?}");
        assert_eq!(wall.connections(), 1);
    }
}
