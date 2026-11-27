# ADR 0009: One TLS Trust Policy for Mail Connections

## Status

Accepted on November 27, 2026 (#471).

## Context

Each protocol trusted a different set of certificate authorities (#435 listed them):

| Client | Trusted |
| --- | --- |
| IMAP (`zmail-imap`) | The system's roots, loaded by `rustls-native-certs` into a store of its own |
| SMTP (`zmail-smtp`, through mail-send) | The operating system's verifier (`rustls-platform-verifier`), a new one per connection |
| JMAP over HTTPS (`jmap-wire`'s `ReqwestTransport`) | reqwest's bundled Mozilla roots (`webpki-roots`) |
| JMAP push over WebSocket (`jmap-wire`'s `TungsteniteTransport`) | The system's roots, loaded by tokio-tungstenite |
| OAuth token requests (`zmail-auth`) | reqwest's bundled Mozilla roots |
| Account discovery (`zmail-autodiscover`) | reqwest's bundled Mozilla roots |

So a server whose certificate comes from a company or private CA that the user had added to the system could work for IMAP and SMTP and fail for JMAP and OAuth, with nothing to say why. Discovery could fail to read a company's autoconfig file that the protocols it found would then have trusted. And the three different loaders differed in what they did with a root they could not parse; IMAP failed the whole connection over one until #435.

## Decision

### The policy

Every connection to a mail server, or to an endpoint that signs in to one or finds one, checks certificates with **the operating system's verifier**, through `rustls-platform-verifier` (what SMTP already used):

- macOS: the Security framework (`SecTrust`), with the user's and the system's keychains and their trust settings.
- Windows: CryptoAPI (`CertGetCertificateChain`), with the machine's and the user's certificate stores, including roots pushed by group policy.
- Linux and the BSDs: WebPKI against the system's root store, read by `rustls-native-certs`: the distribution's bundle, or `SSL_CERT_FILE` and `SSL_CERT_DIR` when they are set. Roots that do not parse are skipped; only a store with no usable root at all is an error.

A CA the user adds to the system is then trusted by every protocol alike, and a CA the system distrusts is distrusted by all of them. Revocation and Apple's and Microsoft's own certificate rules apply where the platform applies them.

### One configuration, shared

`zmail_security::tls::client_config()` builds one rustls `ClientConfig` (aws-lc-rs, TLS 1.2 and 1.3, no client certificate, no ALPN, the platform verifier) the first time a client asks, and hands every caller the same one. A build that fails (on Linux, a system store with no usable root) is not kept, so the next connection tries again. On Linux the system's roots are read when it is built, so a root added later is used after a restart, as IMAP's store was before; macOS and Windows ask the system at every handshake.

| Client | How it gets the configuration |
| --- | --- |
| IMAP | `TlsConnector::from(client_config()?)` |
| SMTP, implicit TLS and STARTTLS | `SmtpClientBuilder::new_with_tls_connector` |
| JMAP over HTTPS: discovery, the session, the API, blobs and the event source | `ReqwestTransport::with_preconfigured_tls` |
| JMAP push over WebSocket | `TungsteniteTransport::with_connector(Connector::Rustls(..))` |
| OAuth token requests | reqwest's `use_preconfigured_tls` |
| Account discovery (autoconfig, ISPDB, the JMAP well-known URL) | reqwest's `use_preconfigured_tls` |

Building it also makes aws-lc-rs rustls's process-wide default provider, which the IMAP, SMTP and JMAP push clients each did before, for any library that builds a rustls configuration of its own.

### `jmap-wire` stays neutral

`jmap-wire` is to be a library of its own (ADR 0008, #517), so it does not depend on `zmail-security` or know this policy. It takes a TLS configuration from its caller through generic constructors: `ReqwestTransport::with_preconfigured_tls`, which hands a `rustls::ClientConfig` to reqwest's `use_preconfigured_tls` and keeps the transport's own settings (no redirects, no `Referer`; anything but a rustls configuration is an error, not the bundled roots), and `TungsteniteTransport::with_connector`, which it already had. Its defaults for other users are unchanged: `ReqwestTransport::new` uses reqwest's bundled Mozilla roots, `TungsteniteTransport::new` the system's roots.

### Where `webpki-roots` remains

The workspace's `reqwest` no longer chooses a root store (`rustls-tls-manual-roots`); a crate that wants reqwest's bundled roots says so in its own manifest. In `cargo tree -e features -p zmail-app -i webpki-roots`, the only feature that brings them in is `reqwest`'s `rustls-tls-webpki-roots`, asked for by:

- `zmail-render-web`, for remote images in HTML mail, fetched only when the user loads them. That is a connection to whatever host a message names, not to a mail server, and this decision leaves it as it was.
- `jmap-wire`, for the default of `ReqwestTransport::new`, which Zmail does not call.

Cargo unifies features, so every `reqwest` client in the app could fall back to the bundled roots if it were built without `use_preconfigured_tls`. The tests below are what catch that.

### Not changed

- **Proton** (`zmail-proton-api`) keeps its own configuration: the system's roots with Proton's SPKI pins on top (ADR 0004). It does not use the shared one.
- No client skips verification: there is no `danger_accept_invalid_certs` or similar anywhere. The one call to rustls's `dangerous()` builder is how rustls takes any verifier other than its built-in WebPKI one; it installs the platform's verifier, which checks fully (`rustls_platform_verifier::BuilderVerifierExt` does the same).
- HTTPS-only stays where it was (discovery, an OAuth endpoint given as `https://`), and so do the redirect policies (none for token requests, at most five HTTPS redirects for discovery, the session's host for JMAP, never for a WebSocket handshake).

## Tests

`zmail-security`'s `test-ca` feature (dev-dependencies only) has a throwaway CA and a server certificate from it for `localhost`, `127.0.0.1` and `::1` (`tls::test_ca`). The CA's private key was deleted once it had signed that certificate, so it vouches for loopback names alone. The shared configuration trusts it on top of the system's only in a process where a test has called `test_ca::trust()`; a build with the feature on but no such call behaves as without it. Each client's test connects to a loopback server with that certificate through the client's own code path, so it passes only if the client uses the shared configuration (each failed with `UnknownIssuer` before this change):

- `zmail-imap`: sign-in over implicit TLS; and in a child process whose `SSL_CERT_FILE` holds a certificate that does not parse and then the CA (Linux and the BSDs), the same sign-in with the CA as a system root.
- `zmail-smtp`: the sign-in check over implicit TLS and over STARTTLS.
- `zmail-jmap`: the sign-in over HTTPS, and WebSocket push with the session over plain HTTP so that only the WebSocket is over TLS.
- `zmail-auth`: a token refresh.
- `zmail-autodiscover`: an HTTPS fetch.
- `zmail-security`: the configuration is built once, the platform verifier refuses the test certificate without the CA, and trusts it with it.

CI runs the workspace tests on Linux, and these tests on macOS and Windows too ("TLS trust tests"), where the verifier is the platform's own.

The test certificates start in 2019 and run to 2125: Apple's verifier refuses a server certificate issued after July 1, 2019 that is valid for more than 825 days, and one that expires would break the tests on a date nobody chose.

## Consequences

- A company or private CA works for every protocol once it is in the system's store, and the user has one place to add it. Bundled roots no longer let a JMAP or OAuth server through that the system does not trust.
- A machine whose system store is empty or unreadable (a minimal container, say) now fails JMAP, OAuth and discovery as it already failed IMAP, instead of falling back to the bundled roots. `SSL_CERT_FILE` fixes it on Linux.
- On Linux a CA added while Zmail runs is used after a restart.
