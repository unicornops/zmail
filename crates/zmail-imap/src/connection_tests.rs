//! Connection timeouts against a local server that stalls (P-4).

use std::time::{Duration, Instant};

use futures_util::TryStreamExt;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use zmail_core::AuthKind;

use crate::{ImapEndpoint, ImapSecurity, connect_session_with, watch::idle_for};

const STALL: Duration = Duration::from_millis(300);

/// A server on a free local port, running `script` on the first connection.
async fn serve<F, Fut>(script: F) -> (ImapEndpoint, JoinHandle<()>)
where
    F: FnOnce(BufReader<TcpStream>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("accept");
        script(BufReader::new(socket)).await;
    });
    let endpoint = ImapEndpoint {
        host: "127.0.0.1".to_owned(),
        port,
        security: ImapSecurity::Plain,
    };
    (endpoint, task)
}

/// Reads one command line and returns its tag.
async fn command(socket: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    socket.read_line(&mut line).await.expect("read command");
    line.split_whitespace()
        .next()
        .expect("a tagged command")
        .to_owned()
}

/// Greets and accepts the LOGIN.
async fn greet_and_login(socket: &mut BufReader<TcpStream>) {
    socket
        .get_mut()
        .write_all(b"* OK ready\r\n")
        .await
        .expect("greet");
    let tag = command(socket).await;
    socket
        .get_mut()
        .write_all(format!("{tag} OK signed in\r\n").as_bytes())
        .await
        .expect("login reply");
}

/// Holds the connection open without sending anything.
async fn stall_for_good(socket: BufReader<TcpStream>) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    drop(socket);
}

async fn sign_in(endpoint: &ImapEndpoint) -> anyhow::Result<()> {
    connect_session_with(endpoint, "user", "secret", AuthKind::Password, STALL)
        .await
        .map(drop)
}

fn assert_stalled(error: &anyhow::Error, started: Instant) {
    let message = format!("{error:#}");
    assert!(
        message.contains("stopped responding"),
        "unexpected error: {message}"
    );
    assert!(started.elapsed() >= STALL);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_server_that_accepts_and_never_greets_fails_the_connection() {
    let (endpoint, server) = serve(stall_for_good).await;
    let started = Instant::now();
    let error = sign_in(&endpoint)
        .await
        .expect_err("a silent server must fail");
    assert_stalled(&error, started);
    server.abort();
}

#[tokio::test]
async fn a_server_that_stalls_on_login_fails_the_sign_in() {
    let (endpoint, server) = serve(|mut socket| async move {
        socket
            .get_mut()
            .write_all(b"* OK ready\r\n")
            .await
            .expect("greet");
        let _ = command(&mut socket).await;
        stall_for_good(socket).await;
    })
    .await;
    let started = Instant::now();
    let error = sign_in(&endpoint)
        .await
        .expect_err("a stalled LOGIN must fail");
    assert_stalled(&error, started);
    server.abort();
}

/// A server that answers `UID FETCH 1 BODY[]` with a body of `chunks`
/// chunks, one every `gap`, and stops after `stop_after` chunks when given.
fn slow_fetch_server(
    chunks: usize,
    gap: Duration,
    stop_after: Option<usize>,
) -> impl FnOnce(BufReader<TcpStream>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
+ Send
+ 'static {
    move |mut socket| {
        Box::pin(async move {
            greet_and_login(&mut socket).await;
            let tag = command(&mut socket).await;
            let chunk = [b'x'; 64];
            socket
                .get_mut()
                .write_all(format!("* 1 FETCH (UID 1 BODY[] {{{}}}\r\n", chunks * 64).as_bytes())
                .await
                .expect("fetch header");
            for sent in 0..chunks {
                if stop_after == Some(sent) {
                    stall_for_good(socket).await;
                    return;
                }
                tokio::time::sleep(gap).await;
                socket.get_mut().write_all(&chunk).await.expect("chunk");
            }
            socket
                .get_mut()
                .write_all(format!(")\r\n{tag} OK done\r\n").as_bytes())
                .await
                .expect("fetch end");
            let _ = command(&mut socket).await;
        })
    }
}

async fn fetch_body(endpoint: &ImapEndpoint) -> anyhow::Result<usize> {
    let mut session =
        connect_session_with(endpoint, "user", "secret", AuthKind::Password, STALL).await?;
    let fetches = session
        .uid_fetch("1", "BODY[]")
        .await?
        .try_collect::<Vec<_>>()
        .await?;
    Ok(fetches
        .first()
        .and_then(|fetch| fetch.body())
        .map_or(0, <[u8]>::len))
}

#[tokio::test]
async fn a_large_fetch_that_keeps_arriving_is_not_cut_off() {
    // Ten chunks a third of the stall timeout apart: three times the
    // timeout in all, without ever stalling.
    let (endpoint, server) = serve(slow_fetch_server(10, STALL / 3, None)).await;
    let started = Instant::now();
    let length = fetch_body(&endpoint)
        .await
        .expect("a moving download completes");
    assert_eq!(length, 640);
    assert!(started.elapsed() > STALL * 2);
    server.abort();
}

#[tokio::test]
async fn a_fetch_that_stops_arriving_fails() {
    let (endpoint, server) = serve(slow_fetch_server(10, STALL / 3, Some(4))).await;
    let started = Instant::now();
    let error = fetch_body(&endpoint)
        .await
        .expect_err("a stalled download must fail");
    assert_stalled(&error, started);
    server.abort();
}

#[tokio::test]
async fn idle_waits_longer_than_the_stall_timeout_and_then_renews() {
    let (endpoint, server) = serve(|mut socket| async move {
        greet_and_login(&mut socket).await;
        let tag = command(&mut socket).await;
        socket
            .get_mut()
            .write_all(b"+ idling\r\n")
            .await
            .expect("idle");
        // DONE arrives untagged once the client renews.
        let mut done = String::new();
        socket.read_line(&mut done).await.expect("done");
        assert_eq!(done.trim(), "DONE");
        socket
            .get_mut()
            .write_all(format!("{tag} OK idle over\r\n").as_bytes())
            .await
            .expect("idle end");
        stall_for_good(socket).await;
    })
    .await;
    let session = connect_session_with(&endpoint, "user", "secret", AuthKind::Password, STALL)
        .await
        .expect("sign in");
    let mut idle = session.idle();
    idle.init().await.expect("IDLE starts");
    let started = Instant::now();
    idle_for(&mut idle, STALL * 3, |_| {})
        .await
        .expect("a quiet IDLE is not a stall");
    assert!(started.elapsed() >= STALL * 3);
    let mut session = idle.done().await.expect("DONE is answered");
    // The stall timeout is back on for the commands after the IDLE.
    let started = Instant::now();
    let error = session.noop().await.expect_err("a stalled NOOP must fail");
    assert!(
        format!("{error:#}").contains("stopped responding"),
        "unexpected error: {error:#}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    server.abort();
}
