//! Submission timeouts against a local server that stalls (P-4).

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::{Duration, Instant},
};

use zmail_core::{OutgoingMessage, SubmissionSecurity};

use crate::{SubmissionConfig, Timeouts, check_sign_in_within, submit_within};

const SHORT: Timeouts = Timeouts {
    connect: Duration::from_millis(300),
    send: Duration::from_millis(300),
};

/// A server on a free local port that runs `script` on the first
/// connection, then keeps the socket open (silent) for a while.
fn serve(script: impl FnOnce(&mut BufReader<TcpStream>) + Send + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    thread::spawn(move || {
        let (socket, _) = listener.accept().expect("accept");
        let mut socket = BufReader::new(socket);
        script(&mut socket);
        thread::sleep(Duration::from_secs(30));
    });
    port
}

fn reply(socket: &mut BufReader<TcpStream>, line: &str) {
    socket
        .get_mut()
        .write_all(format!("{line}\r\n").as_bytes())
        .expect("reply");
}

fn read_line(socket: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    socket.read_line(&mut line).expect("read");
    line
}

fn message() -> OutgoingMessage {
    OutgoingMessage {
        from_email: "me@example.test".to_owned(),
        from_name: None,
        to: vec!["you@example.test".to_owned()],
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: Vec::new(),
        subject: "Hello".to_owned(),
        text_body: "Hi".to_owned(),
        attachments: Vec::new(),
        message_id: None,
        in_reply_to: None,
        references: Vec::new(),
        reply_action: None,
    }
}

fn assert_timely(started: Instant) {
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?}",
        started.elapsed()
    );
}

/// Plain text (the local harness only) is left out: mail-send reads its
/// greeting without a timeout. Over TLS the whole sign-in is timed.
#[tokio::test]
async fn a_server_that_accepts_and_never_greets_fails_the_sign_in_check() {
    for security in [
        SubmissionSecurity::ImplicitTls,
        SubmissionSecurity::StartTls,
    ] {
        let port = serve(|_| {});
        let config = SubmissionConfig {
            security,
            ..SubmissionConfig::plain_local("127.0.0.1", port)
        };
        let started = Instant::now();
        let error = check_sign_in_within(&config, SHORT)
            .await
            .expect_err("a silent server must fail");
        assert!(format!("{error:#}").contains("timeout"), "{error:#}");
        assert_timely(started);
    }
}

#[tokio::test]
async fn a_server_that_never_answers_the_end_of_the_message_fails_the_send() {
    let port = serve(|socket| {
        reply(socket, "220 ready");
        let _ehlo = read_line(socket);
        reply(socket, "250 hello");
        let _mail = read_line(socket);
        reply(socket, "250 ok");
        let _rcpt = read_line(socket);
        reply(socket, "250 ok");
        let _data = read_line(socket);
        reply(socket, "354 go on");
        // Read the message, then never answer it.
        loop {
            let line = read_line(socket);
            if line.is_empty() || line.trim_end() == "." {
                break;
            }
        }
    });
    let started = Instant::now();
    let error = submit_within(
        &SubmissionConfig::plain_local("127.0.0.1", port),
        &message(),
        SHORT,
    )
    .await
    .expect_err("an unanswered message must fail");
    assert!(format!("{error:#}").contains("timeout"), "{error:#}");
    assert_timely(started);
}

#[test]
fn a_bigger_message_gets_longer_to_upload() {
    let timeouts = crate::TIMEOUTS;
    assert_eq!(timeouts.for_message(0), crate::SEND_TIMEOUT);
    // 32 MiB (a 25 MB attachment once base64-encoded, give or take) at
    // 32 KiB/s is about 17 minutes on top of the ten.
    assert_eq!(
        timeouts.for_message(32 * 1024 * 1024),
        crate::SEND_TIMEOUT + Duration::from_secs(1024)
    );
}
