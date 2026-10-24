//! Remote images the user allowed, fetched by Zmail instead of the engine.
//!
//! When the user loads a message's remote content, its public `https`
//! images are served to the page as `zmail-mail://remote/<n>`, and the
//! scheme handler gets them from here. The web view itself never opens a
//! connection, and its content blocker stays deny-all. Each fetch:
//!
//! - accepts only `https` addresses with a host name or a public address;
//! - resolves the host, refuses it if **any** address is not public
//!   (private, loopback, link-local, ...; `zmail_render::is_public_ip`), and
//!   connects to the checked addresses only, so a second lookup cannot
//!   rebind the name to the local network;
//! - sends no cookies, no `Referer` and a generic `User-Agent`;
//! - follows at most [`RemoteLimits::max_redirects`] redirects, each
//!   checked the same way;
//! - accepts only an `image/*` response with status 200, no larger than
//!   [`RemoteLimits::max_bytes`], within [`RemoteLimits::timeout`].
//!
//! Fetches run on a few worker threads ([`RemoteFetcher`]), never on the
//! UI thread.

use std::{
    io::Read,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use url::{Host, Url};
use zmail_render::is_public_ip;

/// What a remote image fetch may cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteLimits {
    /// The largest image accepted, in bytes.
    pub max_bytes: usize,
    /// The longest a fetch may take, redirects included.
    pub timeout: Duration,
    /// How many redirects are followed.
    pub max_redirects: usize,
}

impl Default for RemoteLimits {
    fn default() -> Self {
        Self {
            max_bytes: 5 * 1024 * 1024,
            timeout: Duration::from_secs(15),
            max_redirects: 3,
        }
    }
}

/// A fetched image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteImage {
    /// The response's media type, lowercased, without parameters.
    pub content_type: String,
    /// The image's bytes.
    pub bytes: Vec<u8>,
}

/// Why a remote image was not fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteError {
    /// The address is not an absolute `https` URL with a host.
    NotHttps(String),
    /// The host is, or resolves to, an address that is not public.
    NotPublic(IpAddr),
    /// The host did not resolve.
    Resolve(String),
    /// The request failed (connection, TLS, timeout).
    Request(String),
    /// The server answered with something other than 200 or a redirect.
    Status(u16),
    /// Too many redirects, or a redirect without a usable address.
    Redirect,
    /// The response is not an image.
    NotImage(String),
    /// The image is larger than the limit.
    TooLarge,
    /// The fetch took longer than the limit.
    TimedOut,
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotHttps(url) => write!(formatter, "not an https address: {url}"),
            Self::NotPublic(address) => write!(formatter, "not a public address: {address}"),
            Self::Resolve(error) => write!(formatter, "the host did not resolve: {error}"),
            Self::Request(error) => write!(formatter, "the request failed: {error}"),
            Self::Status(status) => write!(formatter, "the server answered {status}"),
            Self::Redirect => write!(formatter, "too many or bad redirects"),
            Self::NotImage(kind) => write!(formatter, "not an image: {kind}"),
            Self::TooLarge => write!(formatter, "the image is too large"),
            Self::TimedOut => write!(formatter, "the fetch took too long"),
        }
    }
}

impl std::error::Error for RemoteError {}

/// Resolves host names; the system resolver, or a stand-in in tests.
pub trait Resolve: Send + Sync {
    /// The addresses of `host` at `port`.
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String>;
}

/// The system resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
        (host, port)
            .to_socket_addrs()
            .map(Iterator::collect)
            .map_err(|error| error.to_string())
    }
}

/// An `https` address and the checked addresses to connect to for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedTarget {
    /// The address.
    pub url: Url,
    /// The host name (or address) as written.
    pub host: String,
    /// The public addresses the host resolved to.
    pub addresses: Vec<SocketAddr>,
}

/// Checks `url` and resolves its host: `https` only, and every address the
/// host resolves to must be public.
pub fn check_target(url: &str, resolver: &dyn Resolve) -> Result<CheckedTarget, RemoteError> {
    let parsed = Url::parse(url).map_err(|_| RemoteError::NotHttps(url.to_owned()))?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(RemoteError::NotHttps(url.to_owned()));
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    let (host, addresses) = match parsed.host() {
        Some(Host::Ipv4(address)) => (
            address.to_string(),
            vec![SocketAddr::new(address.into(), port)],
        ),
        Some(Host::Ipv6(address)) => (
            address.to_string(),
            vec![SocketAddr::new(address.into(), port)],
        ),
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            let addresses = resolver
                .resolve(&name, port)
                .map_err(RemoteError::Resolve)?;
            (name, addresses)
        }
        None => return Err(RemoteError::NotHttps(url.to_owned())),
    };
    if addresses.is_empty() {
        return Err(RemoteError::Resolve("no addresses".to_owned()));
    }
    if let Some(private) = addresses.iter().find(|address| !is_public_ip(address.ip())) {
        return Err(RemoteError::NotPublic(private.ip()));
    }
    Ok(CheckedTarget {
        url: parsed,
        host,
        addresses,
    })
}

/// Checks a response's media type: `image/*` only. Returns it lowercased,
/// without parameters.
pub fn check_content_type(content_type: Option<&str>) -> Result<String, RemoteError> {
    let kind = content_type
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if kind.starts_with("image/") && kind.len() > "image/".len() {
        Ok(kind)
    } else {
        Err(RemoteError::NotImage(if kind.is_empty() {
            "no content type".to_owned()
        } else {
            kind
        }))
    }
}

/// Fetches the image at `url` with the system resolver.
pub fn fetch_image(url: &str, limits: &RemoteLimits) -> Result<RemoteImage, RemoteError> {
    fetch_image_with(url, limits, &SystemResolver)
}

/// Fetches the image at `url`, resolving with `resolver`.
pub fn fetch_image_with(
    url: &str,
    limits: &RemoteLimits,
    resolver: &dyn Resolve,
) -> Result<RemoteImage, RemoteError> {
    let deadline = Instant::now() + limits.timeout;
    let mut address = url.to_owned();
    for _ in 0..=limits.max_redirects {
        let target = check_target(&address, resolver)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(RemoteError::TimedOut)?;
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .referer(false)
            .no_proxy()
            .timeout(remaining)
            .connect_timeout(remaining.min(Duration::from_secs(10)))
            .user_agent("Mozilla/5.0")
            .resolve_to_addrs(&target.host, &target.addresses)
            .build()
            .map_err(|error| RemoteError::Request(error.to_string()))?;
        let mut response = client
            .get(target.url.clone())
            .header(reqwest::header::ACCEPT, "image/*")
            .send()
            .map_err(|error| {
                if error.is_timeout() {
                    RemoteError::TimedOut
                } else {
                    RemoteError::Request(error.to_string())
                }
            })?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(RemoteError::Redirect)?;
            address = target
                .url
                .join(location)
                .map_err(|_| RemoteError::Redirect)?
                .to_string();
            continue;
        }
        if status.as_u16() != 200 {
            return Err(RemoteError::Status(status.as_u16()));
        }
        let content_type = check_content_type(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
        )?;
        if response
            .content_length()
            .is_some_and(|length| length > limits.max_bytes as u64)
        {
            return Err(RemoteError::TooLarge);
        }
        let mut bytes = Vec::new();
        (&mut response)
            .take(limits.max_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| RemoteError::Request(error.to_string()))?;
        if bytes.len() > limits.max_bytes {
            return Err(RemoteError::TooLarge);
        }
        return Ok(RemoteImage {
            content_type,
            bytes,
        });
    }
    Err(RemoteError::Redirect)
}

/// A fetch's result, for the scheme handler.
pub type FetchResult = Result<RemoteImage, RemoteError>;

type Job = (String, Sender<FetchResult>);

/// A few worker threads that fetch remote images.
pub struct RemoteFetcher {
    jobs: Mutex<Sender<Job>>,
}

impl RemoteFetcher {
    /// How many images are fetched at once.
    const WORKERS: usize = 4;

    /// The process's fetcher, started on first use.
    pub fn shared() -> &'static Self {
        static FETCHER: OnceLock<RemoteFetcher> = OnceLock::new();
        FETCHER.get_or_init(Self::start)
    }

    fn start() -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        let queue = Arc::new(Mutex::new(queue));
        for index in 0..Self::WORKERS {
            let queue = Arc::clone(&queue);
            let spawned = thread::Builder::new()
                .name(format!("zmail-remote-images-{index}"))
                .spawn(move || {
                    let limits = RemoteLimits::default();
                    loop {
                        let job = match queue.lock() {
                            Ok(queue) => queue.recv(),
                            Err(_) => return,
                        };
                        let Ok((url, reply)) = job else {
                            return;
                        };
                        let _ = reply.send(fetch_image(&url, &limits));
                    }
                });
            if let Err(error) = spawned {
                eprintln!("zmail: web renderer: could not start an image fetcher: {error}");
            }
        }
        Self {
            jobs: Mutex::new(jobs),
        }
    }

    /// Queues `url`; its result arrives on the returned receiver.
    pub fn fetch(&self, url: String) -> Receiver<FetchResult> {
        let (reply, result) = mpsc::channel();
        let queued = self
            .jobs
            .lock()
            .map(|jobs| jobs.send((url, reply.clone())).is_ok())
            .unwrap_or(false);
        if !queued {
            let _ = reply.send(Err(RemoteError::Request(
                "the image fetcher is not running".to_owned(),
            )));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    /// Resolves every name to fixed addresses, and counts nothing: any test
    /// that reaches the network fails on its own.
    struct Fixed(Vec<IpAddr>);

    impl Resolve for Fixed {
        fn resolve(&self, _host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
            Ok(self.0.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
        }
    }

    fn public() -> Fixed {
        Fixed(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))])
    }

    #[test]
    fn only_https_addresses_are_fetched() {
        for url in [
            "http://img.example.com/a.png",
            "ftp://img.example.com/a.png",
            "file:///etc/passwd",
            "https://user:pw@img.example.com/a.png",
            "not a url",
        ] {
            assert!(
                matches!(check_target(url, &public()), Err(RemoteError::NotHttps(_))),
                "{url}"
            );
        }
        let target = check_target("https://IMG.example.com./a.png", &public()).expect("public");
        assert_eq!(target.host, "img.example.com");
        assert_eq!(target.addresses[0].port(), 443);
    }

    #[test]
    fn a_host_with_any_private_address_is_refused() {
        for private in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            "fe80::1".parse().expect("an address"),
            "fd00::1".parse().expect("an address"),
        ] {
            let resolver = Fixed(vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), private]);
            assert_eq!(
                check_target("https://img.example.com/a.png", &resolver),
                Err(RemoteError::NotPublic(private)),
                "{private}"
            );
        }
        assert!(matches!(
            check_target("https://127.0.0.1/a.png", &public()),
            Err(RemoteError::NotPublic(_))
        ));
        assert!(matches!(
            check_target("https://[::1]/a.png", &public()),
            Err(RemoteError::NotPublic(_))
        ));
        assert!(matches!(
            check_target("https://img.example.com/a.png", &Fixed(Vec::new())),
            Err(RemoteError::Resolve(_))
        ));
    }

    #[test]
    fn refused_addresses_never_reach_the_network() {
        // fetch_image_with stops at the check, before building a client.
        let resolver = Fixed(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        assert_eq!(
            fetch_image_with(
                "https://img.example.com/a.png",
                &RemoteLimits::default(),
                &resolver
            ),
            Err(RemoteError::NotPublic(IpAddr::V4(Ipv4Addr::LOCALHOST)))
        );
    }

    #[test]
    fn only_image_responses_are_accepted() {
        assert_eq!(
            check_content_type(Some("Image/PNG; charset=binary")),
            Ok("image/png".to_owned())
        );
        assert_eq!(
            check_content_type(Some("image/svg+xml")),
            Ok("image/svg+xml".to_owned())
        );
        for other in [Some("text/html"), Some("image/"), Some(""), None] {
            assert!(
                matches!(check_content_type(other), Err(RemoteError::NotImage(_))),
                "{other:?}"
            );
        }
    }
}
