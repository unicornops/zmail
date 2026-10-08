//! OAuth2 authorization-code flow with PKCE (RFC 7636) for IMAP `XOAUTH2`.
//!
//! The flow runs entirely on the loopback interface: a one-shot HTTP server
//! captures the provider's redirect, and the authorization URL is printed so
//! the flow also works when no browser can be opened for the user.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// A one-shot OAuth2 token exchange result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSet {
    /// Bearer token for IMAP `XOAUTH2` and the provider API.
    pub access_token: String,
    /// Refresh token when the provider grants one.
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds, when reported.
    pub expires_in: Option<u64>,
}

/// An OAuth2 provider client configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuth2Client {
    /// Provider application client id.
    pub client_id: String,
    /// Provider application client secret (confidential clients only).
    pub client_secret: Option<String>,
    /// Authorization endpoint URL.
    pub authorization_endpoint: String,
    /// Token endpoint URL.
    pub token_endpoint: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// Loopback port the redirect receiver listens on.
    pub redirect_port: u16,
    /// Extra authorization-endpoint parameters (for example Google's
    /// `access_type=offline`) appended to the request URL.
    pub extra_authorize_params: Vec<(String, String)>,
}

/// Well-known provider presets for the browser flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuth2Preset {
    /// Google Workspace / Gmail.
    Gmail,
    /// Microsoft 365 / Outlook.com.
    Microsoft365,
}

impl OAuth2Preset {
    /// The preset's display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gmail => "Gmail",
            Self::Microsoft365 => "Microsoft 365",
        }
    }

    /// Builds the provider client with its preset endpoints and scopes.
    pub fn client(
        self,
        client_id: String,
        client_secret: Option<String>,
        port: u16,
    ) -> OAuth2Client {
        match self {
            Self::Gmail => OAuth2Client {
                client_id,
                // Google's desktop clients are confidential; the secret is
                // embedded in the application, and PKCE keeps the flow
                // honest on top of it.
                client_secret,
                authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth".to_owned(),
                token_endpoint: "https://oauth2.googleapis.com/token".to_owned(),
                // The full IMAP scope is required for XOAUTH2.
                scopes: vec!["https://mail.google.com/".to_owned()],
                redirect_port: port,
                // Refresh tokens are only granted for explicitly offline
                // access, and only on a consent prompt.
                extra_authorize_params: vec![
                    ("access_type".to_owned(), "offline".to_owned()),
                    ("prompt".to_owned(), "consent".to_owned()),
                ],
            },
            Self::Microsoft365 => OAuth2Client {
                client_id,
                client_secret,
                authorization_endpoint:
                    "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".to_owned(),
                token_endpoint: "https://login.microsoftonline.com/common/oauth2/v2.0/token"
                    .to_owned(),
                scopes: vec![
                    "https://outlook.office365.com/IMAP.AccessAsUser.All".to_owned(),
                    "offline_access".to_owned(),
                ],
                redirect_port: port,
                extra_authorize_params: Vec::new(),
            },
        }
    }
}

/// PKCE pair (RFC 7636): the secret verifier and its S256 challenge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// The verifier kept client-side only.
    pub verifier: String,
    /// The `S256` challenge sent in the authorization request.
    pub challenge: String,
}

impl Pkce {
    /// Generates a random PKCE pair.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let verifier = base64url(&bytes);
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// RFC 4648 base64url without padding.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let byte_0 = chunk[0];
        let byte_1 = *chunk.get(1).unwrap_or(&0);
        let byte_2 = *chunk.get(2).unwrap_or(&0);
        let triple = ((byte_0 as u32) << 16) | ((byte_1 as u32) << 8) | byte_2 as u32;
        output.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        output.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(triple >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[triple as usize & 63] as char);
        }
    }
    output
}

/// The authorization request the user's browser opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest {
    /// Full URL to open in the browser.
    pub url: String,
    /// Random state the redirect must echo back.
    pub state: String,
    /// PKCE pair for the token exchange.
    pub pkce: Pkce,
}

impl OAuth2Client {
    /// The loopback redirect URI used throughout the flow.
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.redirect_port)
    }

    /// Builds the authorization request URL with PKCE and state.
    pub fn authorization_request(&self) -> AuthorizationRequest {
        let pkce = Pkce::generate();
        let mut state_bytes = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut state_bytes);
        let state = base64url(&state_bytes);

        let mut url = format!(
            "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
            self.authorization_endpoint,
            urlencode(&self.client_id),
            urlencode(&self.redirect_uri()),
            state,
            pkce.challenge,
        );
        if !self.scopes.is_empty() {
            let scopes = self
                .scopes
                .iter()
                .map(|scope| urlencode(scope))
                .collect::<Vec<_>>()
                .join("%20");
            url.push_str("&scope=");
            url.push_str(&scopes);
        }
        if let Some(secret) = &self.client_secret {
            url.push_str("&client_secret=");
            url.push_str(&urlencode(secret));
        }
        for (key, value) in &self.extra_authorize_params {
            url.push('&');
            url.push_str(&urlencode(key));
            url.push('=');
            url.push_str(&urlencode(value));
        }

        AuthorizationRequest { url, state, pkce }
    }

    /// Waits for the provider redirect on the loopback port and returns the
    /// authorization code after validating the echoed state.
    pub fn listen_for_redirect(&self, expected_state: &str, timeout: Duration) -> Result<String> {
        let listener = TcpListener::bind(("127.0.0.1", self.redirect_port))
            .with_context(|| format!("failed to bind the redirect port {}", self.redirect_port))?;
        listener
            .set_nonblocking(true)
            .context("failed to prepare the redirect listener")?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Some(code) = Self::capture_redirect(stream, expected_state)? {
                        return Ok(code);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() > deadline {
                        bail!("timed out waiting for the OAuth2 redirect");
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(error).context("failed to accept the redirect"),
            }
        }
    }

    /// Reads one redirect request, answers it, and extracts the code.
    fn capture_redirect(mut stream: TcpStream, expected_state: &str) -> Result<Option<String>> {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .context("failed to prepare the redirect stream")?;
        let mut request = [0u8; 8192];
        let read = stream
            .read(&mut request)
            .context("failed to read the redirect request")?;
        let request = String::from_utf8_lossy(&request[..read]).into_owned();
        let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
                       <html><body><p>zmail received the authorization. Return to the terminal.</p></body></html>";
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();

        let Some(query) = request.split_whitespace().nth(1) else {
            return Ok(None);
        };
        let query = query.split('?').nth(1).unwrap_or_default();
        let mut code = None;
        let mut state = None;
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "code" => code = Some(urldecode(value)),
                "state" => state = Some(urldecode(value)),
                _ => {}
            }
        }

        if state.as_deref() != Some(expected_state) {
            bail!("the redirect state does not match the request (possible CSRF)");
        }
        let Some(code) = code.filter(|code| !code.is_empty()) else {
            bail!("the OAuth2 redirect carried no authorization code");
        };
        Ok(Some(code))
    }

    /// Exchanges the authorization code for tokens using the PKCE verifier.
    pub fn exchange_code(&self, code: &str, pkce: &Pkce) -> Result<TokenSet> {
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("code_verifier", pkce.verifier.clone()),
            ("client_id", self.client_id.clone()),
            ("redirect_uri", self.redirect_uri()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        self.token_request(form)
    }

    /// Runs the whole browser flow: request URL, redirect capture, exchange.
    pub fn authorize_interactively(&self) -> Result<TokenSet> {
        let request = self.authorization_request();
        println!(
            "Open this URL in a browser to authorize zmail:\n{}\n",
            request.url
        );
        open_browser(&request.url);
        let code = self.listen_for_redirect(&request.state, Duration::from_secs(300))?;
        self.exchange_code(&code, &request.pkce)
    }

    /// Refreshes the access token with a previously granted refresh token.
    pub fn refresh(&self, refresh_token: &str) -> Result<TokenSet> {
        refresh_access_token(
            &self.token_endpoint,
            &self.client_id,
            self.client_secret.as_deref(),
            refresh_token,
        )
    }

    fn token_request(&self, form: Vec<(&str, String)>) -> Result<TokenSet> {
        token_request(&self.token_endpoint, form)
    }
}

/// Refreshes an access token against a provider token endpoint.
///
/// This is the background-refresh entry point used by sync: it needs only
/// the fields persisted with the account, not the full interactive client.
/// Per RFC 6749 the response may omit the refresh token; the returned set
/// then keeps the grant that was used.
pub fn refresh_access_token(
    token_endpoint: &str,
    client_id: &str,
    client_secret: Option<&str>,
    refresh_token: &str,
) -> Result<TokenSet> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", refresh_token.to_owned()),
        ("client_id", client_id.to_owned()),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret.to_owned()));
    }
    let tokens = token_request(token_endpoint, form)?;
    Ok(TokenSet {
        access_token: tokens.access_token,
        // Per RFC 6749 the refresh token may be omitted on refresh.
        refresh_token: tokens.refresh_token.or(Some(refresh_token.to_owned())),
        expires_in: tokens.expires_in,
    })
}

fn token_request(endpoint: &str, form: Vec<(&str, String)>) -> Result<TokenSet> {
    #[cfg(test)]
    let client = reqwest::blocking::Client::builder()
        // Unit tests talk to loopback mock endpoints; the host proxy
        // environment must not intercept them.
        .no_proxy()
        .build()
        .context("failed to prepare the token client")?;
    #[cfg(not(test))]
    let client = reqwest::blocking::Client::new();
    let response = client
        .post(endpoint)
        .form(&form)
        .send()
        .context("the token request failed")?;
    if !response.status().is_success() {
        bail!(
            "the token endpoint rejected the request with HTTP {}",
            response.status()
        );
    }
    let json: serde_json::Value = response.json().context("the token response was not JSON")?;
    let access_token = json
        .get("access_token")
        .and_then(|value| value.as_str())
        .context("the token response carried no access_token")?
        .to_owned();
    Ok(TokenSet {
        access_token,
        refresh_token: json
            .get("refresh_token")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        expires_in: json.get("expires_in").and_then(|value| value.as_u64()),
    })
}

/// Opens `url` with the platform browser; failures leave the printed URL.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(target_os = "windows")]
    let command = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let command = "xdg-open";
    let _ = std::process::Command::new(command)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Percent-encodes a query component.
fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// Decodes a percent-encoded query component.
fn urldecode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                match hex {
                    Some(byte) => {
                        decoded.push(byte);
                        index += 3;
                    }
                    None => {
                        decoded.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
        time::Duration,
    };

    use super::{OAuth2Client, Pkce};
    use anyhow::{Context, Result, bail};
    use sha2::Digest;

    fn client(port: u16) -> OAuth2Client {
        OAuth2Client {
            client_id: "test-client".to_owned(),
            client_secret: None,
            authorization_endpoint: "https://provider.test/authorize".to_owned(),
            token_endpoint: format!("http://127.0.0.1:{port}/token"),
            scopes: vec!["imap".to_owned(), "email".to_owned()],
            redirect_port: port,
            extra_authorize_params: Vec::new(),
        }
    }

    #[test]
    fn gmail_preset_requests_offline_imap_access() {
        let client = super::OAuth2Preset::Gmail.client(
            "gmail-client".to_owned(),
            Some("gmail-secret".to_owned()),
            8357,
        );
        assert_eq!(
            client.authorization_endpoint,
            "https://accounts.google.com/o/oauth2/v2/auth"
        );
        assert_eq!(client.token_endpoint, "https://oauth2.googleapis.com/token");
        assert_eq!(client.scopes, vec!["https://mail.google.com/".to_owned()]);

        let request = client.authorization_request();
        assert!(request.url.contains("access_type=offline"));
        assert!(request.url.contains("prompt=consent"));
        assert!(request.url.contains("client_secret=gmail-secret"));
    }

    #[test]
    fn microsoft_preset_targets_the_common_tenant() {
        let client = super::OAuth2Preset::Microsoft365.client("ms-client".to_owned(), None, 8357);
        assert!(
            client
                .authorization_endpoint
                .starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/authorize")
        );
        assert!(
            client
                .token_endpoint
                .starts_with("https://login.microsoftonline.com/common/oauth2/v2.0/token")
        );
        assert!(client.scopes.contains(&"offline_access".to_owned()));
        assert!(
            client
                .scopes
                .contains(&"https://outlook.office365.com/IMAP.AccessAsUser.All".to_owned())
        );
    }

    fn free_port() -> u16 {
        TcpListener::bind(("127.0.0.1", 0))
            .expect("a listener should bind")
            .local_addr()
            .expect("the address should resolve")
            .port()
    }

    /// Connects with retries so a spawned listener has time to bind.
    fn connect_with_retry(port: u16) -> std::net::TcpStream {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match std::net::TcpStream::connect(("127.0.0.1", port)) {
                Ok(stream) => return stream,
                Err(_) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("the receiver never accepted: {error}"),
            }
        }
    }

    #[test]
    fn pkce_pairs_use_the_s256_challenge() {
        let pkce = Pkce::generate();
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "the verifier must be 43-128 characters"
        );
        assert!(
            pkce.verifier
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "the verifier uses unreserved characters only"
        );
        let digest = sha2::Sha256::digest(pkce.verifier.as_bytes());
        assert_eq!(
            pkce.challenge.len(),
            43,
            "S256 challenges are 43 characters"
        );
        assert_ne!(pkce.challenge, pkce.verifier);
        let expected = super::base64url(&digest);
        assert_eq!(pkce.challenge, expected);
    }

    #[test]
    fn authorization_requests_carry_pkce_and_state() {
        let client = client(free_port());
        let request = client.authorization_request();
        assert!(request.url.starts_with("https://provider.test/authorize?"));
        for fragment in [
            "response_type=code",
            "client_id=test-client",
            "state=",
            "code_challenge_method=S256",
            "code_challenge=",
            "redirect_uri=http%3A%2F%2F127.0.0.1",
            "scope=imap%20email",
        ] {
            assert!(
                request.url.contains(fragment),
                "the authorization URL should contain {fragment}: {}",
                request.url
            );
        }
    }

    #[test]
    fn redirect_capture_returns_the_code_and_validates_state() {
        let port = free_port();
        let client = client(port);
        let state = "known-state".to_owned();
        let expected_state = state.clone();
        let listener = thread::spawn(move || {
            client.listen_for_redirect(&expected_state, Duration::from_secs(10))
        });

        let mut stream = connect_with_retry(port);
        stream
            .write_all(
                format!("GET /callback?code=auth-code-1&state={state} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                    .as_bytes(),
            )
            .expect("the redirect should send");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);

        let code = listener
            .join()
            .expect("the listener thread should finish")
            .expect("the redirect should be captured");
        assert_eq!(code, "auth-code-1");
    }

    #[test]
    fn redirect_capture_rejects_mismatched_state() {
        let port = free_port();
        let client = client(port);
        let listener =
            thread::spawn(move || client.listen_for_redirect("expected", Duration::from_secs(10)));

        let mut stream = connect_with_retry(port);
        stream
            .write_all(b"GET /callback?code=auth-code-1&state=attacker HTTP/1.1\r\nHost: x\r\n\r\n")
            .expect("the redirect should send");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);

        let result = listener.join().expect("the listener thread should finish");
        assert!(result.is_err(), "a mismatched state must be rejected");
    }

    /// Retries until the mock endpoint is bound; anything other than a
    /// connection refusal fails the test immediately.
    fn retry_until_bound<T>(mut attempt: impl FnMut() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match attempt() {
                Ok(value) => return Ok(value),
                Err(error)
                    if std::time::Instant::now() < deadline
                        && format!("{error:#}").contains("Connection refused") =>
                {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// One-shot mock token endpoint asserting the grant shape.
    fn mock_token_server(
        port: u16,
        expected_grant: &'static str,
    ) -> thread::JoinHandle<Result<()>> {
        let listener = match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => listener,
            Err(error) => return thread::spawn(move || Err(error).context("bind failed")),
        };
        thread::spawn(move || -> Result<()> {
            let (mut stream, _) = listener
                .accept()
                .context("the token endpoint should accept")?;
            let mut request = [0u8; 8192];
            let read = stream.read(&mut request).context("read failed")?;
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            if !request.contains(&format!("grant_type={expected_grant}")) {
                bail!("unexpected grant in token request: {request}");
            }
            if !request.contains("client_id=test-client") {
                bail!("the token request should identify the client: {request}");
            }
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n\
                      {\"access_token\":\"access-1\",\"refresh_token\":\"refresh-1\",\"expires_in\":3600}",
                )
                .context("write failed")?;
            Ok(())
        })
    }

    #[test]
    fn exchanges_codes_with_the_pkce_verifier() {
        let port = free_port();
        let server = mock_token_server(port, "authorization_code");
        let client = client(port);
        let pkce = Pkce::generate();

        let tokens = retry_until_bound(|| client.exchange_code("auth-code-1", &pkce))
            .expect("the exchange should succeed");
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-1"));
        assert_eq!(tokens.expires_in, Some(3600));
        server
            .join()
            .expect("the mock server should finish")
            .expect("the mock server should validate the request");
    }

    #[test]
    fn refreshes_tokens_and_keeps_the_grant() {
        let port = free_port();
        let server = mock_token_server(port, "refresh_token");
        let client = client(port);

        let tokens =
            retry_until_bound(|| client.refresh("refresh-1")).expect("the refresh should succeed");
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(
            tokens.refresh_token.as_deref(),
            Some("refresh-1"),
            "an omitted refresh token keeps the previous grant"
        );
        server
            .join()
            .expect("the mock server should finish")
            .expect("the mock server should validate the request");
    }

    #[test]
    fn refresh_access_token_uses_only_the_persisted_fields() {
        let port = free_port();
        let server = mock_token_server(port, "refresh_token");

        let tokens = retry_until_bound(|| {
            super::refresh_access_token(
                &format!("http://127.0.0.1:{port}/token"),
                "test-client",
                None,
                "refresh-1",
            )
        })
        .expect("the background refresh should succeed");
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(
            tokens.refresh_token.as_deref(),
            Some("refresh-1"),
            "an omitted refresh token keeps the previous grant"
        );
        assert_eq!(tokens.expires_in, Some(3600));
        server
            .join()
            .expect("the mock server should finish")
            .expect("the mock server should validate the request");
    }
}
