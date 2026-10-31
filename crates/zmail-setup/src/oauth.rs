//! The OAuth2 browser sign-in, step by step: the authorization URL (with
//! PKCE and a random state, from `zmail-auth`), the loopback listener that
//! catches the provider's redirect, and the token exchange.
//!
//! `zmail_auth::OAuth2Client::authorize_interactively` does all three in
//! one call and prints the URL to standard output, which suits the CLI and
//! the GUI's background thread. A terminal UI owns the screen, so it uses
//! [`BrowserSignIn`] instead: it shows [`BrowserSignIn::url`] itself (and
//! opens it with the system's opener), then waits with
//! [`BrowserSignIn::wait`] off its event loop.
//!
//! The redirect goes to `127.0.0.1` on the machine running Zmail, so it
//! only works when the browser runs there too. Over SSH it can't reach a
//! remote terminal UI ([`REMOTE_LOOPBACK_NOTE`]). Zmail doesn't offer the
//! device flow instead: Google doesn't allow the Gmail scope with it.
//!
//! Zmail ships no OAuth client ID or secret of its own: the user brings
//! their own client (`crate::google` has the guide for Gmail).

use std::time::Duration;

use anyhow::{Context as _, Result};
use zmail_auth::{AuthorizationRequest, OAuth2Client, TokenSet};

/// How long the loopback listener waits for the browser, as
/// `authorize_interactively` does.
pub const REDIRECT_TIMEOUT: Duration = Duration::from_secs(300);

/// What a terminal UI says when it runs over SSH: the browser's redirect
/// goes to the computer the browser runs on, not to this one.
pub const REMOTE_LOOPBACK_NOTE: &str = "This terminal is connected over SSH. Your browser sends the sign-in back to 127.0.0.1 on the computer the browser runs on, so it can't reach Zmail on this machine. Use an app password instead, or add the account on the computer you're sitting at.";

/// An OAuth2 browser sign-in under way: its authorization request, made
/// once, and the client it is for.
pub struct BrowserSignIn {
    client: OAuth2Client,
    request: AuthorizationRequest,
}

impl std::fmt::Debug for BrowserSignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSignIn")
            .field("url", &self.request.url)
            .finish_non_exhaustive()
    }
}

impl BrowserSignIn {
    /// Starts a sign-in for `client`: a fresh state and PKCE pair.
    pub fn new(client: OAuth2Client) -> Self {
        let request = client.authorization_request();
        Self { client, request }
    }

    /// The URL the user opens in a browser. It carries the client ID, the
    /// redirect address, the scopes and the PKCE challenge, never the
    /// client secret.
    pub fn url(&self) -> &str {
        &self.request.url
    }

    /// The loopback address the provider redirects to.
    pub fn redirect_uri(&self) -> String {
        self.client.redirect_uri()
    }

    /// Waits for the redirect (up to `timeout`), checks its state and
    /// exchanges the code for tokens. Blocks; run it off the UI thread.
    pub fn wait(self, timeout: Duration) -> Result<(OAuth2Client, TokenSet)> {
        let code = self
            .client
            .listen_for_redirect(&self.request.state, timeout)
            .context("the browser sign-in didn't finish")?;
        let tokens = self
            .client
            .exchange_code(&code, &self.request.pkce)
            .context("the provider didn't accept the sign-in")?;
        Ok((self.client, tokens))
    }
}

/// Whether this process runs in an SSH session (`SSH_CONNECTION` or
/// `SSH_TTY` is set), where the browser redirect can't reach it.
pub fn over_ssh() -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;

    use zmail_auth::OAuth2Client;

    use super::BrowserSignIn;

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .map(|address| address.port())
            .expect("a free port")
    }

    fn client(port: u16) -> OAuth2Client {
        OAuth2Client {
            client_id: "client-1".to_owned(),
            client_secret: Some("not-in-the-url".to_owned()),
            authorization_endpoint: "https://provider.test/authorize".to_owned(),
            token_endpoint: "http://127.0.0.1:9/token".to_owned(),
            scopes: vec!["email".to_owned()],
            redirect_port: port,
            extra_authorize_params: Vec::new(),
        }
    }

    #[test]
    fn the_url_carries_pkce_and_state_but_never_the_secret() {
        let sign_in = BrowserSignIn::new(client(8357));
        let url = sign_in.url();
        assert!(url.starts_with("https://provider.test/authorize?"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state="));
        assert!(!url.contains("not-in-the-url"));
        assert_eq!(sign_in.redirect_uri(), "http://127.0.0.1:8357/callback");
        assert!(!format!("{sign_in:?}").contains("not-in-the-url"));
    }

    #[test]
    fn a_redirect_with_the_wrong_state_is_refused() {
        let port = free_port();
        let sign_in = BrowserSignIn::new(client(port));
        let waiting = std::thread::spawn(move || sign_in.wait(Duration::from_secs(10)));
        // The listener binds when waiting starts; retry until it does.
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(connected) = TcpStream::connect(("127.0.0.1", port)) {
                stream = Some(connected);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut stream = stream.expect("the listener is up");
        stream
            .write_all(b"GET /callback?code=abc&state=forged HTTP/1.1\r\n\r\n")
            .expect("the redirect is sent");
        let mut answer = String::new();
        let _ = stream.read_to_string(&mut answer);
        let error = waiting
            .join()
            .expect("the wait ends")
            .expect_err("a forged state is refused");
        assert!(format!("{error:#}").contains("state"), "{error:#}");
    }
}
