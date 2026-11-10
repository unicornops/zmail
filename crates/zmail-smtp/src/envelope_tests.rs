//! The SMTP envelope against a server on loopback that records every
//! command line it receives: an address that could not stand in
//! `MAIL FROM:<...>` or `RCPT TO:<...>` as it is never reaches the server.

use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::{Arc, Mutex},
};

use zmail_core::{AuthKind, OutgoingMessage, SubmissionSecurity};

use crate::{SubmissionConfig, submit_outgoing_message};

/// An SMTP server that accepts everything and records the command lines
/// (not the message data) of every session.
struct RecordingServer {
    port: u16,
    commands: Arc<Mutex<Vec<String>>>,
}

impl RecordingServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        let commands = Arc::new(Mutex::new(Vec::new()));
        let log = commands.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().expect("the stream clones"));
                let _ = stream.write_all(b"220 localhost ESMTP\r\n");
                let mut in_data = false;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if in_data {
                        if line == ".\r\n" {
                            in_data = false;
                            let _ = stream.write_all(b"250 2.0.0 queued\r\n");
                        }
                        continue;
                    }
                    log.lock()
                        .expect("the log")
                        .push(line.trim_end().to_owned());
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        b"250-localhost\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("DATA") {
                        in_data = true;
                        b"354 go ahead\r\n"
                    } else if upper.starts_with("QUIT") {
                        let _ = stream.write_all(b"221 bye\r\n");
                        break;
                    } else {
                        b"250 2.0.0 ok\r\n"
                    };
                    let _ = stream.write_all(reply);
                }
            }
        });
        Self { port, commands }
    }

    fn config(&self) -> SubmissionConfig {
        SubmissionConfig {
            host: "127.0.0.1".to_owned(),
            port: self.port,
            username: None,
            secret: None,
            auth: AuthKind::Password,
            security: SubmissionSecurity::Plain,
            hello_name: "localhost".to_owned(),
        }
    }

    fn commands(&self) -> Vec<String> {
        self.commands.lock().expect("the log").clone()
    }
}

fn message(from: &str, to: &[&str], bcc: &[&str]) -> OutgoingMessage {
    OutgoingMessage {
        from_email: from.to_owned(),
        from_name: None,
        to: to.iter().map(|&entry| entry.to_owned()).collect(),
        cc: Vec::new(),
        bcc: bcc.iter().map(|&entry| entry.to_owned()).collect(),
        reply_to: Vec::new(),
        subject: "Hello".to_owned(),
        text_body: "Body".to_owned(),
        attachments: Vec::new(),
        message_id: None,
        in_reply_to: None,
        references: Vec::new(),
        reply_action: None,
    }
}

#[tokio::test]
async fn valid_addresses_are_sent_as_bare_envelope_addresses() {
    let server = RecordingServer::start();
    submit_outgoing_message(
        &server.config(),
        &message(
            "me@example.com",
            &["Kai Santos <kai@example.com>"],
            &["hidden@example.com"],
        ),
    )
    .await
    .expect("sends");
    let commands = server.commands();
    assert!(
        commands.contains(&"MAIL FROM:<me@example.com>".to_owned()),
        "{commands:?}"
    );
    assert!(
        commands.contains(&"RCPT TO:<kai@example.com>".to_owned()),
        "{commands:?}"
    );
    assert!(
        commands.contains(&"RCPT TO:<hidden@example.com>".to_owned()),
        "{commands:?}"
    );
}

#[tokio::test]
async fn a_recipient_with_a_line_break_never_reaches_the_server() {
    let server = RecordingServer::start();
    let error = submit_outgoing_message(
        &server.config(),
        &message(
            "me@example.com",
            &["kai@example.com"],
            &["a@example.com>\r\nRCPT TO:<other@example.net"],
        ),
    )
    .await
    .expect_err("refused before connecting");
    assert!(
        error
            .to_string()
            .contains("Not an address Zmail can send to"),
        "{error}"
    );
    assert!(
        error.to_string().contains("\\r\\n"),
        "the line break is shown, not acted on: {error}"
    );
    assert!(server.commands().is_empty(), "{:?}", server.commands());
}

#[tokio::test]
async fn a_sender_with_a_line_break_never_reaches_the_server() {
    let server = RecordingServer::start();
    let error = submit_outgoing_message(
        &server.config(),
        &message("me@example.com>\r\nRSET", &["kai@example.com"], &[]),
    )
    .await
    .expect_err("refused before connecting");
    assert!(
        error
            .to_string()
            .contains("Not an address Zmail can send from"),
        "{error}"
    );
    assert!(server.commands().is_empty(), "{:?}", server.commands());
}
