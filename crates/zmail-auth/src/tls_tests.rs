//! Token requests trust what the shared TLS configuration trusts
//! (`zmail_security::tls`, ADR 0009).

use zmail_security::tls::test_ca;

use crate::refresh_access_token;

#[test]
fn shared_tls_token_requests_trust_what_the_shared_configuration_trusts() {
    test_ca::trust();
    let body = r#"{"access_token":"fresh","expires_in":3600}"#;
    let (port, server) = test_ca::serve_https_once(format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    ));
    let tokens = refresh_access_token(
        &format!("https://localhost:{port}/token"),
        "client",
        None,
        "refresh",
    );
    let request = server.join().expect("the server thread");
    let tokens = tokens.expect("the token request trusts a certificate the configuration trusts");
    assert_eq!(tokens.access_token, "fresh");
    let request = request.expect("the server read the request");
    assert!(request.starts_with("POST /token "), "{request}");
    assert!(request.contains("grant_type=refresh_token"), "{request}");
}
