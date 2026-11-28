//! Discovery's HTTPS fetches trust what the shared TLS configuration
//! trusts (`zmail_security::tls`, ADR 0009).

use url::Url;
use zmail_security::tls::test_ca;

use crate::{Fetch, fetch::HttpsFetcher};

#[test]
fn shared_tls_discovery_trusts_what_the_shared_configuration_trusts() {
    test_ca::trust();
    let (port, server) = test_ca::serve_https_once(
        "HTTP/1.1 200 OK\r\nContent-Type: text/xml\r\nContent-Length: 2\r\n\
         Connection: close\r\n\r\nok",
    );
    let url = Url::parse(&format!("https://localhost:{port}/mail/config-v1.1.xml")).expect("URL");
    let fetched = HttpsFetcher::new().get(&url);
    let request = server.join().expect("the server thread");
    let response = fetched.expect("discovery trusts a certificate the configuration trusts");
    assert_eq!(
        (response.status, response.body.as_slice()),
        (200, &b"ok"[..])
    );
    let request = request.expect("the server read the request");
    assert!(
        request.starts_with("GET /mail/config-v1.1.xml "),
        "{request}"
    );
}
