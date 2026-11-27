//! SMTP over implicit TLS and STARTTLS trusts what the shared TLS
//! configuration trusts (`zmail_security::tls`, ADR 0009).

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    thread,
};

use zmail_core::SubmissionSecurity;
use zmail_security::tls::test_ca;

use crate::{SubmissionConfig, check_sign_in};

fn reply(socket: &mut BufReader<impl Read + Write>, line: &str) -> io::Result<()> {
    socket
        .get_mut()
        .write_all(format!("{line}\r\n").as_bytes())?;
    socket.get_mut().flush()
}

fn read_line(socket: &mut BufReader<impl Read + Write>) -> io::Result<String> {
    let mut line = String::new();
    socket.read_line(&mut line)?;
    Ok(line)
}

/// Greets, answers EHLO and says goodbye to QUIT: a sign-in check with
/// no credentials.
fn converse(socket: &mut BufReader<impl Read + Write>) -> io::Result<()> {
    reply(socket, "220 localhost ready")?;
    let _ehlo = read_line(socket)?;
    reply(socket, "250 localhost")?;
    let _quit = read_line(socket)?;
    reply(socket, "221 bye")
}

/// A submission server on a free loopback port with the test CA's
/// certificate. The thread ends with an error if the client refuses it.
fn serve(security: SubmissionSecurity) -> (u16, thread::JoinHandle<io::Result<()>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("address").port();
    let server = thread::spawn(move || {
        if security == SubmissionSecurity::ImplicitTls {
            return converse(&mut BufReader::new(test_ca::accept(&listener)?));
        }
        let (tcp, _) = listener.accept()?;
        let mut plain = BufReader::new(tcp);
        reply(&mut plain, "220 localhost ready")?;
        let _ehlo = read_line(&mut plain)?;
        reply(&mut plain, "250-localhost")?;
        reply(&mut plain, "250 STARTTLS")?;
        let _starttls = read_line(&mut plain)?;
        reply(&mut plain, "220 go ahead")?;
        let mut tls = BufReader::new(test_ca::handshake(plain.into_inner())?);
        let _ehlo = read_line(&mut tls)?;
        reply(&mut tls, "250 localhost")?;
        let _quit = read_line(&mut tls)?;
        reply(&mut tls, "221 bye")
    });
    (port, server)
}

#[tokio::test]
async fn shared_tls_smtp_trusts_what_the_shared_configuration_trusts() {
    test_ca::trust();
    for security in [
        SubmissionSecurity::ImplicitTls,
        SubmissionSecurity::StartTls,
    ] {
        let (port, server) = serve(security);
        let config = SubmissionConfig {
            security,
            ..SubmissionConfig::plain_local("localhost", port)
        };
        let checked = check_sign_in(&config).await;
        let served = server.join().expect("the server thread");
        checked.unwrap_or_else(|error| {
            panic!(
                "{security:?}: SMTP signs in with a certificate the configuration trusts: {error:#}"
            )
        });
        served.unwrap_or_else(|error| panic!("{security:?}: the server's side: {error}"));
    }
}
