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

use anyhow::{Context, Result, anyhow, bail};
use rand::{Rng, rand_core::UnwrapErr, rngs::SysRng};
use sha2::{Digest, Sha256};

/// A one-shot OAuth2 token exchange result.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenSet {
    /// Bearer token for IMAP `XOAUTH2` and the provider API.
    pub access_token: String,
    /// Refresh token when the provider grants one.
    pub refresh_token: Option<String>,
    /// Lifetime of the access token in seconds, when reported.
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &REDACTED)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| REDACTED),
            )
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// What `Debug` shows in place of a secret.
const REDACTED: &str = "[redacted]";

/// A token endpoint's refusal (RFC 6749 §5.2): the HTTP status and, when
/// the body carries them, the error code and its description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenError {
    /// The HTTP status.
    pub status: u16,
    /// The OAuth2 error code, such as `invalid_grant`.
    pub error: Option<String>,
    /// The provider's description of the error.
    pub description: Option<String>,
}

impl TokenError {
    /// The refresh token (or authorization code) is no longer accepted:
    /// it expired or was revoked, and only signing in again helps. Google
    /// answers this for every refresh once a Testing app's 7-day sign-in
    /// has lapsed.
    pub fn is_invalid_grant(&self) -> bool {
        self.error.as_deref() == Some("invalid_grant")
    }

    /// The `error_code: description` pair, for messages.
    pub fn summary(&self) -> String {
        match (&self.error, &self.description) {
            (Some(error), Some(description)) => format!("{error}: {description}"),
            (Some(error), None) => error.clone(),
            (None, _) => format!("HTTP {}", self.status),
        }
    }

    /// The token error somewhere in `error`'s chain, if any.
    pub fn find(error: &anyhow::Error) -> Option<&Self> {
        error.chain().find_map(|cause| cause.downcast_ref())
    }
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the token endpoint rejected the request with HTTP {}",
            self.status
        )?;
        if self.error.is_some() {
            write!(f, " ({})", self.summary())?;
        }
        Ok(())
    }
}

impl std::error::Error for TokenError {}

/// An OAuth2 provider client configuration.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuth2Client {
    /// Provider application client id.
    pub client_id: String,
    /// Provider application client secret (confidential clients only).
    /// It is sent to the token endpoint only, never in the browser's
    /// authorization URL.
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

impl std::fmt::Debug for OAuth2Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuth2Client")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| REDACTED),
            )
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("scopes", &self.scopes)
            .field("redirect_port", &self.redirect_port)
            .field("extra_authorize_params", &self.extra_authorize_params)
            .finish()
    }
}

/// Well-known provider presets for the browser flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuth2Preset {
    /// Google Workspace / Gmail.
    Gmail,
    /// Microsoft 365 / Outlook.com.
    Microsoft365,
}

/// The scopes the Microsoft 365 preset asks for.
pub const MICROSOFT_365_SCOPES: [&str; 3] = [
    "https://outlook.office.com/IMAP.AccessAsUser.All",
    "https://outlook.office.com/SMTP.Send",
    "offline_access",
];

/// The local port the browser flow's redirect comes back to by default.
pub const DEFAULT_REDIRECT_PORT: u16 = 8357;

impl OAuth2Preset {
    /// The preset whose token endpoint is `token_endpoint`, so a stored
    /// account's client can be rebuilt to sign in again.
    pub fn for_token_endpoint(token_endpoint: &str) -> Option<Self> {
        [Self::Gmail, Self::Microsoft365]
            .into_iter()
            .find(|preset| preset.client(String::new(), None, 0).token_endpoint == token_endpoint)
    }

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
                // IMAP for reading, SMTP.Send for XOAUTH2 submission
                // through smtp.office365.com, both on Exchange Online's
                // `outlook.office.com` resource as Microsoft documents
                // them (one token covers one resource). Accounts signed in
                // before SMTP.Send was asked for keep their grant until
                // they sign in again: a refresh never widens it, and a
                // send refused for it asks the user to Re-authenticate.
                scopes: MICROSOFT_365_SCOPES
                    .iter()
                    .map(|scope| (*scope).to_owned())
                    .collect(),
                redirect_port: port,
                extra_authorize_params: Vec::new(),
            },
        }
    }
}

/// PKCE pair (RFC 7636): the secret verifier and its S256 challenge.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    /// The verifier kept client-side only.
    pub verifier: String,
    /// The `S256` challenge sent in the authorization request.
    pub challenge: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The challenge is public (it is in the authorization URL); the
        // verifier is what proves the code is ours.
        f.debug_struct("Pkce")
            .field("verifier", &REDACTED)
            .field("challenge", &self.challenge)
            .finish()
    }
}

impl Pkce {
    /// Generates a random PKCE pair.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        UnwrapErr(SysRng).fill_bytes(&mut bytes);
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
        UnwrapErr(SysRng).fill_bytes(&mut state_bytes);
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
        // The client secret never goes in this URL: the browser, its
        // history and any redirect logs would all see it. Only the token
        // endpoint request carries it.
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
        self.listen_for_redirect_until(expected_state, timeout, || false)
    }

    /// [`Self::listen_for_redirect`], giving up (and freeing the port) as
    /// soon as `cancelled` returns true, so a UI can stop waiting when the
    /// user leaves the sign-in.
    pub fn listen_for_redirect_until(
        &self,
        expected_state: &str,
        timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<String> {
        self.listen(expected_state, timeout, CALLBACK_READ_TIMEOUT, cancelled)
    }

    /// The loopback listener. Only the provider's redirect, to the
    /// callback path and with this sign-in's state, ends the wait (with
    /// the code, or with the provider's error). Anything else that reaches
    /// the port (another page, a stale or forged state, a connection that
    /// sends nothing within `read_timeout`) gets a 400 and the wait goes
    /// on, until the real redirect or the deadline.
    fn listen(
        &self,
        expected_state: &str,
        timeout: Duration,
        read_timeout: Duration,
        cancelled: impl Fn() -> bool,
    ) -> Result<String> {
        let listener = TcpListener::bind(("127.0.0.1", self.redirect_port))
            .with_context(|| format!("failed to bind the redirect port {}", self.redirect_port))?;
        listener
            .set_nonblocking(true)
            .context("failed to prepare the redirect listener")?;

        let deadline = std::time::Instant::now() + timeout;
        loop {
            if cancelled() {
                bail!("the sign-in was cancelled");
            }
            if std::time::Instant::now() > deadline {
                bail!("timed out waiting for the OAuth2 redirect");
            }
            match listener.accept() {
                Ok((stream, _)) => match capture_redirect(stream, expected_state, read_timeout) {
                    Callback::Code(code) => return Ok(code),
                    Callback::Failed(error) => return Err(error),
                    Callback::Ignored => {}
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(error).context("failed to accept the redirect"),
            }
        }
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
        // The CLI's custom client reaches here without the setup form's
        // checks; the token endpoint is checked on every request.
        check_endpoint(&self.authorization_endpoint)?;
        let request = self.authorization_request();
        println!(
            "Open this URL in a browser to authorize Zmail:\n{}\n",
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

/// How long one connection to the redirect listener may take to send its
/// request.
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The path of the loopback redirect URI.
const CALLBACK_PATH: &str = "/callback";

/// What one connection to the redirect listener brought.
#[derive(Debug)]
enum Callback {
    /// The provider's redirect with the authorization code.
    Code(String),
    /// The provider's redirect saying the sign-in failed.
    Failed(anyhow::Error),
    /// Anything else, answered with a 400 and otherwise ignored.
    Ignored,
}

/// Reads one request from the redirect listener, answers it, and says
/// what it was.
fn capture_redirect(
    mut stream: TcpStream,
    expected_state: &str,
    read_timeout: Duration,
) -> Callback {
    // An accepted socket can inherit the listener's non-blocking mode.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(read_timeout)).is_err()
    {
        return Callback::Ignored;
    }
    let mut request = [0u8; 8192];
    let callback = match stream.read(&mut request) {
        Ok(read) => parse_callback(&String::from_utf8_lossy(&request[..read]), expected_state),
        Err(_) => Callback::Ignored,
    };
    let _ = stream.write_all(callback_page(&callback).as_bytes());
    let _ = stream.flush();
    callback
}

/// What a redirect request means for the sign-in waiting on
/// `expected_state`.
fn parse_callback(request: &str, expected_state: &str) -> Callback {
    let mut request_line = request.split_whitespace();
    let (Some("GET"), Some(target)) = (request_line.next(), request_line.next()) else {
        return Callback::Ignored;
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return Callback::Ignored;
    }
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => code = Some(urldecode(value)),
            "state" => state = Some(urldecode(value)),
            "error" => error = Some(urldecode(value)),
            "error_description" => description = Some(urldecode(value)),
            _ => {}
        }
    }
    // Only this sign-in's state may end it, with a code or an error; a
    // forged or stale request is ignored.
    if state.as_deref() != Some(expected_state) {
        return Callback::Ignored;
    }
    // RFC 6749 section 4.1.2.1: the provider reports a refusal (the user
    // said no, say) as `error` on the redirect.
    if let Some(error) = error {
        let refusal = match description.filter(|description| !description.is_empty()) {
            Some(description) => format!("{error}: {description}"),
            None => error,
        };
        return Callback::Failed(anyhow!("the provider did not grant access ({refusal})"));
    }
    match code.filter(|code| !code.is_empty()) {
        Some(code) => Callback::Code(code),
        None => Callback::Failed(anyhow!(
            "the provider's redirect carried no authorization code"
        )),
    }
}

/// The page the browser shows after a request to the redirect listener.
fn callback_page(callback: &Callback) -> String {
    let (status, text) = match callback {
        Callback::Code(_) => (
            "200 OK",
            "Zmail has your sign-in. You can close this tab and go back to Zmail.",
        ),
        Callback::Failed(_) => (
            "200 OK",
            "The sign-in did not finish. You can close this tab; Zmail says what went wrong.",
        ),
        Callback::Ignored => (
            "400 Bad Request",
            "This address is only for signing in to Zmail, and this request was not expected.",
        ),
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Zmail</title></head><body><p>{text}</p></body></html>"
    );
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Checks that an OAuth2 endpoint is safe to send credentials to: `https`,
/// or `http` on the loopback interface only (a local test server).
pub fn check_endpoint(endpoint: &str) -> Result<()> {
    let url = reqwest::Url::parse(endpoint.trim())
        .with_context(|| format!("{endpoint} is not a valid URL"))?;
    let host = url
        .host_str()
        .unwrap_or_default()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    match url.scheme() {
        "https" if !host.is_empty() => Ok(()),
        "http" if loopback => Ok(()),
        _ => bail!("the OAuth2 endpoint {endpoint} must be an https:// address"),
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
    check_endpoint(endpoint)?;
    let endpoint = endpoint.trim();
    // The request carries the client secret, the code or the refresh
    // token: it never follows a redirect (a 307 would post them on to
    // another host), and an https endpoint never falls back to http.
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(endpoint.starts_with("https://"));
    // Unit tests talk to loopback mock endpoints; the host proxy
    // environment must not intercept them.
    #[cfg(test)]
    let client = client.no_proxy();
    let client = client
        .build()
        .context("failed to prepare the token client")?;
    let response = client
        .post(endpoint)
        .form(&form)
        .send()
        .context("the token request failed")?;
    let status = response.status();
    if !status.is_success() {
        // RFC 6749 §5.2: the body names the error, such as `invalid_grant`
        // for a refresh token that expired or was revoked.
        let body: serde_json::Value = response.json().unwrap_or_default();
        let field = |name: &str| {
            body.get(name)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        return Err(TokenError {
            status: status.as_u16(),
            error: field("error"),
            description: field("error_description"),
        }
        .into());
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

    use super::{OAuth2Client, OAuth2Preset, Pkce};
    use anyhow::{Context, Result, bail};
    use sha2::Digest;

    #[test]
    fn presets_are_found_by_their_token_endpoint() {
        for preset in [OAuth2Preset::Gmail, OAuth2Preset::Microsoft365] {
            let endpoint = preset.client("id".to_owned(), None, 1).token_endpoint;
            assert_eq!(OAuth2Preset::for_token_endpoint(&endpoint), Some(preset));
        }
        assert_eq!(
            OAuth2Preset::for_token_endpoint("https://auth.example.test/token"),
            None
        );
    }

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
    }

    #[test]
    fn authorization_urls_never_carry_the_client_secret() {
        for preset in [OAuth2Preset::Gmail, OAuth2Preset::Microsoft365] {
            let client = preset.client(
                "client-id".to_owned(),
                Some("very-secret-value".to_owned()),
                8357,
            );
            let url = client.authorization_request().url;
            assert!(
                !url.contains("client_secret"),
                "{} must not name the secret: {url}",
                preset.name()
            );
            assert!(
                !url.contains("very-secret-value"),
                "{} must not carry the secret: {url}",
                preset.name()
            );
        }
    }

    #[test]
    fn the_token_exchange_carries_the_client_secret() {
        let port = free_port();
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("the mock binds");
        let server = thread::spawn(move || -> Result<String> {
            let (mut stream, _) = listener.accept().context("accept")?;
            let mut request = [0u8; 8192];
            let read = stream.read(&mut request).context("read")?;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n\
                      {\"access_token\":\"access-1\"}",
                )
                .context("write")?;
            Ok(String::from_utf8_lossy(&request[..read]).into_owned())
        });
        let mut client = client(port);
        client.client_secret = Some("very-secret-value".to_owned());

        retry_until_bound(|| client.exchange_code("auth-code-1", &Pkce::generate()))
            .expect("the exchange should succeed");
        let request = server
            .join()
            .expect("the mock finishes")
            .expect("the mock reads the request");
        assert!(
            request.contains("client_secret=very-secret-value"),
            "the token request sends the secret: {request}"
        );
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
        assert_eq!(
            client.scopes,
            [
                "https://outlook.office.com/IMAP.AccessAsUser.All",
                "https://outlook.office.com/SMTP.Send",
                "offline_access",
            ],
            "IMAP, XOAUTH2 sending and a refresh token, on one resource"
        );
        let url = client.authorization_request().url;
        assert!(
            url.contains("SMTP.Send"),
            "the sign-in asks for sending: {url}"
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

    /// Sends `request` to the redirect listener and returns its answer.
    fn send_to_listener(port: u16, request: &str) -> String {
        let mut stream = connect_with_retry(port);
        stream
            .write_all(request.as_bytes())
            .expect("the request should send");
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    #[test]
    fn a_mismatched_state_is_refused_and_the_wait_goes_on() {
        let port = free_port();
        let client = client(port);
        let listener =
            thread::spawn(move || client.listen_for_redirect("expected", Duration::from_secs(10)));

        let forged = send_to_listener(
            port,
            "GET /callback?code=auth-code-1&state=attacker HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(forged.starts_with("HTTP/1.1 400"), "{forged}");
        assert!(
            !listener.is_finished(),
            "a forged state must not end the wait"
        );

        let real = send_to_listener(
            port,
            "GET /callback?code=auth-code-2&state=expected HTTP/1.1\r\nHost: x\r\n\r\n",
        );
        assert!(real.starts_with("HTTP/1.1 200"), "{real}");
        let code = listener
            .join()
            .expect("the listener thread should finish")
            .expect("the real redirect still arrives");
        assert_eq!(code, "auth-code-2");
    }

    #[test]
    fn stray_requests_do_not_end_the_sign_in() {
        let port = free_port();
        let client = client(port);
        let listener = thread::spawn(move || {
            client.listen(
                "expected",
                Duration::from_secs(10),
                Duration::from_millis(300),
                || false,
            )
        });

        // Another page, as a web page's <img src> or a favicon would ask.
        let other_page = send_to_listener(port, "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n");
        assert!(other_page.starts_with("HTTP/1.1 400"), "{other_page}");
        // The callback without any state, and an error with a forged one.
        for request in [
            "GET /callback HTTP/1.1\r\n\r\n",
            "GET /callback?error=access_denied&state=forged HTTP/1.1\r\n\r\n",
            "POST /callback?code=c&state=expected HTTP/1.1\r\n\r\n",
            "not http at all",
        ] {
            let answer = send_to_listener(port, request);
            assert!(answer.starts_with("HTTP/1.1 400"), "{request}: {answer}");
        }
        // A connection that sends nothing until the read times out.
        let silent = connect_with_retry(port);
        thread::sleep(Duration::from_millis(500));
        drop(silent);
        assert!(
            !listener.is_finished(),
            "stray requests must not end the wait"
        );

        send_to_listener(
            port,
            "GET /callback?code=auth-code-3&state=expected HTTP/1.1\r\n\r\n",
        );
        let code = listener
            .join()
            .expect("the listener thread should finish")
            .expect("the real redirect still arrives");
        assert_eq!(code, "auth-code-3");
    }

    #[test]
    fn only_stray_requests_end_at_the_deadline() {
        let port = free_port();
        let client = client(port);
        let listener =
            thread::spawn(move || client.listen_for_redirect("expected", Duration::from_secs(1)));
        send_to_listener(port, "GET /callback?state=attacker HTTP/1.1\r\n\r\n");
        let error = listener
            .join()
            .expect("the listener thread should finish")
            .expect_err("no real redirect came");
        assert!(format!("{error:#}").contains("timed out"), "{error:#}");
    }

    #[test]
    fn a_provider_refusal_is_reported_as_what_it_is() {
        let port = free_port();
        let client = client(port);
        let listener =
            thread::spawn(move || client.listen_for_redirect("expected", Duration::from_secs(10)));
        let page = send_to_listener(
            port,
            "GET /callback?error=access_denied&error_description=The+user+said+no&state=expected HTTP/1.1\r\n\r\n",
        );
        let error = listener
            .join()
            .expect("the listener thread should finish")
            .expect_err("a refusal ends the sign-in");
        let message = format!("{error:#}");
        assert!(message.contains("access_denied"), "{message}");
        assert!(message.contains("The user said no"), "{message}");
        assert!(!message.contains("CSRF"), "{message}");
        assert!(page.contains("did not finish"), "{page}");
        assert!(page.contains("Zmail"), "{page}");
    }

    #[test]
    fn the_browser_page_is_neutral_and_names_zmail() {
        for (request, expected) in [
            (
                "GET /callback?code=c&state=s HTTP/1.1",
                "Zmail has your sign-in",
            ),
            ("GET /callback?error=e&state=s HTTP/1.1", "did not finish"),
            ("GET /other HTTP/1.1", "only for signing in to Zmail"),
        ] {
            let page = super::callback_page(&super::parse_callback(request, "s"));
            assert!(page.contains(expected), "{request}: {page}");
            assert!(!page.contains("terminal"), "{page}");
            assert!(
                !page.contains("zmail "),
                "the product name is Zmail: {page}"
            );
            let length = page
                .split("Content-Length: ")
                .nth(1)
                .and_then(|rest| rest.split("\r\n").next())
                .and_then(|value| value.parse::<usize>().ok())
                .expect("a content length");
            let body = page.split("\r\n\r\n").nth(1).expect("a body");
            assert_eq!(length, body.len());
        }
    }

    #[test]
    fn oauth_endpoints_must_be_https_except_on_loopback() {
        for good in [
            "https://login.example.com/oauth2/token",
            " https://login.example.com/authorize ",
            "http://127.0.0.1:8080/token",
            "http://[::1]:8080/token",
            "http://localhost/token",
        ] {
            assert!(super::check_endpoint(good).is_ok(), "{good}");
        }
        for bad in [
            "http://login.example.com/oauth2/token",
            "http://127.0.0.1.example.com/token",
            "ftp://login.example.com/token",
            "login.example.com/token",
            "https://",
            "",
        ] {
            assert!(super::check_endpoint(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_token_request_refuses_cleartext_and_redirects() {
        let error = super::refresh_access_token(
            "http://login.example.com/token",
            "test-client",
            Some("secret"),
            "refresh-1",
        )
        .expect_err("cleartext is refused before anything is sent");
        assert!(format!("{error:#}").contains("https"), "{error:#}");

        // A token endpoint that redirects elsewhere: the secret is not
        // posted on, and the redirect is a failure.
        let port = free_port();
        let server = mock_token_error(
            port,
            "307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/elsewhere",
            "",
        );
        let error = retry_until_bound(|| {
            super::refresh_access_token(
                &format!("http://127.0.0.1:{port}/token"),
                "test-client",
                Some("secret"),
                "refresh-1",
            )
        })
        .expect_err("a redirect is not followed");
        server.join().expect("the mock finishes");
        let token_error = super::TokenError::find(&error).expect("a token error");
        assert_eq!(token_error.status, 307);
    }

    #[test]
    fn debug_output_never_shows_secrets() {
        let tokens = super::TokenSet {
            access_token: "access-secret".to_owned(),
            refresh_token: Some("refresh-secret".to_owned()),
            expires_in: Some(3600),
        };
        let mut client = client(8357);
        client.client_secret = Some("client-secret".to_owned());
        let pkce = Pkce::generate();
        let request = client.authorization_request();
        for (output, secrets) in [
            (
                format!("{tokens:?}"),
                vec!["access-secret", "refresh-secret"],
            ),
            (format!("{client:?}"), vec!["client-secret"]),
            (format!("{pkce:?}"), vec![pkce.verifier.as_str()]),
            (format!("{request:?}"), vec![request.pkce.verifier.as_str()]),
        ] {
            for secret in secrets {
                assert!(!output.contains(secret), "{output}");
            }
            assert!(output.contains("[redacted]"), "{output}");
        }
        assert!(format!("{client:?}").contains("test-client"));
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

    /// One-shot mock token endpoint answering `status` with `body`.
    fn mock_token_error(
        port: u16,
        status: &'static str,
        body: &'static str,
    ) -> thread::JoinHandle<()> {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("the mock binds");
        thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 8192];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
        })
    }

    #[test]
    fn an_expired_or_revoked_refresh_token_is_an_invalid_grant() {
        let port = free_port();
        let server = mock_token_error(
            port,
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        );
        let error = retry_until_bound(|| {
            super::refresh_access_token(
                &format!("http://127.0.0.1:{port}/token"),
                "test-client",
                Some("secret"),
                "refresh-1",
            )
        })
        .expect_err("the refresh is refused");
        server.join().expect("the mock finishes");
        let token_error = super::TokenError::find(&error).expect("a token error");
        assert!(token_error.is_invalid_grant());
        assert_eq!(token_error.status, 400);
        assert_eq!(
            token_error.summary(),
            "invalid_grant: Token has been expired or revoked."
        );
        assert!(format!("{error:#}").contains("invalid_grant"));
    }

    #[test]
    fn other_token_failures_are_not_invalid_grants() {
        for (status, body) in [
            ("400 Bad Request", r#"{"error":"invalid_client"}"#),
            ("500 Internal Server Error", "<html>oops</html>"),
        ] {
            let port = free_port();
            let server = mock_token_error(port, status, body);
            let error = retry_until_bound(|| {
                super::refresh_access_token(
                    &format!("http://127.0.0.1:{port}/token"),
                    "test-client",
                    None,
                    "refresh-1",
                )
            })
            .expect_err("the refresh is refused");
            server.join().expect("the mock finishes");
            let token_error = super::TokenError::find(&error).expect("a token error");
            assert!(!token_error.is_invalid_grant(), "{status}: {body}");
        }
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
