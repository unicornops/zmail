//! One Zmail window per store for `mailto:` links (#457).
//!
//! The system's mail link handler runs `zmail-app mailto:...` for every
//! link clicked (the Linux desktop entry and the Windows registration;
//! macOS sends the link to the running app itself). While Zmail is open,
//! that second launch hands its link to the running app, which opens a
//! compose window on it, and quits; only when no Zmail is running does it
//! start the window itself, as before.
//!
//! The channel is local and private to the user and the store's data
//! directory (`ZMAIL_DATA_DIR` included), so a Zmail on another store is
//! another instance:
//!
//! - Unix (Linux, macOS): a Unix domain socket `zmail-<hash>.sock` in the
//!   user's runtime directory (`XDG_RUNTIME_DIR` on Linux, the per-user
//!   `TMPDIR` on macOS) when it is the user's alone, else in `run/` in the
//!   data directory, made private (0700). The socket is 0600, and the
//!   running app checks each connection's peer is its own user.
//! - Windows: a named pipe `\\.\pipe\ie.unicornops.zmail-<hash>`, created
//!   as the first instance of its name (no other process can be serving
//!   it already) and refusing remote clients. Its default security lets
//!   only the user who created it (and administrators and SYSTEM) write
//!   to it; anyone else can only fail to send.
//!
//! `<hash>` is the start of the SHA-256 of the data directory (and on
//! Windows the user name).
//!
//! What goes through it is one line: a `mailto:` link, at most
//! [`MAX_LINK_BYTES`]. The running app accepts it only when
//! [`parse_mailto`] does, answers `ok` or `refused`, and never does
//! anything else with what it receives. A socket left behind by a Zmail
//! that died refuses connections: the next launch starts the window and
//! replaces it.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use zmail_security::mailto::parse_mailto;

/// The longest link the running app reads, in bytes.
pub(crate) const MAX_LINK_BYTES: usize = 256 * 1024;

/// How long either side waits for the other.
const TIMEOUT: Duration = Duration::from_secs(5);

/// How long the running app waits for the other end to close after
/// answering.
const LINGER: Duration = Duration::from_secs(1);

/// The running app's answer to a link it opens.
const ACCEPTED: &[u8] = b"ok\n";
/// Its answer to anything else.
const REFUSED: &[u8] = b"refused\n";

/// Where the running app listens for links: a socket path on Unix, a pipe
/// name on Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint(PathBuf);

impl Endpoint {
    /// The endpoint of the store in `data_dir`, or `None` when there is
    /// no private place for it (then every launch starts its own window).
    pub(crate) fn for_data_dir(data_dir: &Path) -> Option<Self> {
        let hash = endpoint_hash(data_dir);
        platform::endpoint(data_dir, &hash).map(Self)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

/// A short hash naming the store in `data_dir` (and on Windows its user).
fn endpoint_hash(data_dir: &Path) -> String {
    // The same store under another spelling of its path is the same store.
    let _ = std::fs::create_dir_all(data_dir);
    let dir = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    let mut hasher = Sha256::new();
    hasher.update(dir.to_string_lossy().as_bytes());
    if cfg!(windows) {
        // Pipe names are machine-wide.
        hasher.update(b"\0");
        hasher.update(std::env::var("USERNAME").unwrap_or_default().as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What became of a link handed to a running app.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum HandOff {
    /// The running app took it.
    Delivered,
    /// No app took it: none is running, the one there died, refused or
    /// did not answer in time. The caller opens the window itself.
    NotDelivered,
}

/// Hands `link` to the Zmail running on `endpoint`, if one is.
pub(crate) fn hand_off(endpoint: &Endpoint, link: &str) -> HandOff {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return HandOff::NotDelivered;
    };
    runtime.block_on(async {
        match tokio::time::timeout(TIMEOUT, send(endpoint, link)).await {
            Ok(Ok(true)) => HandOff::Delivered,
            _ => HandOff::NotDelivered,
        }
    })
}

/// Sends `link` and reads the answer: whether it was accepted.
async fn send(endpoint: &Endpoint, link: &str) -> io::Result<bool> {
    let mut stream = platform::connect(endpoint).await?;
    stream.write_all(link.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    Ok(read_answer(&mut stream).await? == ACCEPTED)
}

/// The running app's one-line answer, with its newline.
async fn read_answer<S: AsyncRead + Unpin>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut answer = Vec::new();
    BufReader::new(stream.take(16))
        .read_until(b'\n', &mut answer)
        .await?;
    Ok(answer)
}

/// Receives links for the running app until dropped.
pub(crate) struct Listener {
    task: tokio::task::JoinHandle<()>,
    #[cfg(unix)]
    path: PathBuf,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Starts receiving links on `endpoint`, on `runtime`, calling `open` with
/// each `mailto:` link [`parse_mailto`] accepts (it says whether the app
/// took it). `Ok(None)` when another Zmail already receives them there.
pub(crate) fn listen(
    endpoint: &Endpoint,
    runtime: &tokio::runtime::Handle,
    open: impl Fn(String) -> bool + Send + Sync + 'static,
) -> io::Result<Option<Listener>> {
    let open: Arc<dyn Fn(String) -> bool + Send + Sync> = Arc::new(open);
    let _guard = runtime.enter();
    platform::listen(endpoint, runtime, open)
}

/// Reads one link from `stream`, opens it when it is one and answers.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    open: Arc<dyn Fn(String) -> bool + Send + Sync>,
) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut line = Vec::new();
    let mut reader = BufReader::new(reader.take(MAX_LINK_BYTES as u64 + 1));
    let read = reader.read_until(b'\n', &mut line);
    let accepted = match tokio::time::timeout(TIMEOUT, read).await {
        Ok(Ok(_)) => accept(&line).is_some_and(|link| open(link)),
        _ => false,
    };
    let answer = if accepted { ACCEPTED } else { REFUSED };
    let answered = tokio::time::timeout(TIMEOUT, async {
        writer.write_all(answer).await?;
        writer.flush().await
    })
    .await;
    if !matches!(answered, Ok(Ok(()))) {
        return;
    }
    // The other end closes once it has the answer; until then the
    // connection stays open, so the answer isn't lost with it (a named
    // pipe has no half-close).
    let mut rest = reader.into_inner().into_inner();
    let _ = tokio::time::timeout(LINGER, async {
        let mut sink = [0; 512];
        while rest.read(&mut sink).await.is_ok_and(|read| read > 0) {}
    })
    .await;
}

/// The link in `line` (one line, without its newline), when it is a
/// `mailto:` link the parser accepts and no longer than
/// [`MAX_LINK_BYTES`].
fn accept(line: &[u8]) -> Option<String> {
    let line = line.strip_suffix(b"\n")?;
    if line.len() > MAX_LINK_BYTES {
        return None;
    }
    let link = std::str::from_utf8(line).ok()?;
    parse_mailto(link)?;
    Some(link.to_owned())
}

#[cfg(unix)]
mod platform {
    use std::{
        io,
        os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _},
        path::{Path, PathBuf},
        sync::Arc,
    };

    use tokio::net::{UnixListener, UnixStream};

    use super::{Endpoint, Listener, serve};

    /// The socket's path: in the user's runtime directory when it is
    /// private, else in a private `run/` in the data directory.
    pub(super) fn endpoint(data_dir: &Path, hash: &str) -> Option<PathBuf> {
        let name = format!("zmail-{hash}.sock");
        let runtime_dir = if cfg!(target_os = "macos") {
            std::env::var_os("TMPDIR")
        } else {
            std::env::var_os("XDG_RUNTIME_DIR")
        };
        if let Some(dir) = runtime_dir.map(PathBuf::from)
            && dir.is_absolute()
            && is_private(&dir)
        {
            return Some(dir.join(name));
        }
        let run = data_dir.join("run");
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&run);
        is_private(&run).then(|| run.join(name))
    }

    /// Whether `dir` is a directory of this user's that no one else can
    /// write to or list.
    fn is_private(dir: &Path) -> bool {
        std::fs::metadata(dir).is_ok_and(|metadata| {
            metadata.is_dir()
                && metadata.uid() == rustix::process::getuid().as_raw()
                && metadata.mode() & 0o077 == 0
        })
    }

    pub(super) async fn connect(endpoint: &Endpoint) -> io::Result<UnixStream> {
        UnixStream::connect(&endpoint.0).await
    }

    pub(super) fn listen(
        endpoint: &Endpoint,
        runtime: &tokio::runtime::Handle,
        open: Arc<dyn Fn(String) -> bool + Send + Sync>,
    ) -> io::Result<Option<Listener>> {
        let path = endpoint.0.clone();
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                // A Zmail that answers is running; a socket that refuses
                // was left by one that died.
                if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                    return Ok(None);
                }
                std::fs::remove_file(&path)?;
                UnixListener::bind(&path)?
            }
            Err(error) => return Err(error),
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let uid = rustix::process::getuid().as_raw();
        let task = runtime.spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    // Out of file descriptors, say: try again shortly
                    // rather than spin.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                };
                // Only this user's processes may hand over a link.
                if !stream.peer_cred().is_ok_and(|peer| peer.uid() == uid) {
                    continue;
                }
                tokio::spawn(serve(stream, open.clone()));
            }
        });
        Ok(Some(Listener { task, path }))
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        io,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    };

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    use super::{Endpoint, Listener, serve};

    /// `ERROR_PIPE_BUSY`: every instance of the pipe is serving a client.
    const ERROR_PIPE_BUSY: i32 = 231;

    pub(super) fn endpoint(_data_dir: &Path, hash: &str) -> Option<PathBuf> {
        Some(PathBuf::from(format!(
            r"\\.\pipe\ie.unicornops.zmail-{hash}"
        )))
    }

    pub(super) async fn connect(endpoint: &Endpoint) -> io::Result<NamedPipeClient> {
        loop {
            match ClientOptions::new().open(&endpoint.0) {
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                result => return result,
            }
        }
    }

    /// A new instance of the pipe; the first must be the first of its
    /// name, so no other process already serves it.
    fn instance(name: &Path, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(name)
    }

    pub(super) fn listen(
        endpoint: &Endpoint,
        runtime: &tokio::runtime::Handle,
        open: Arc<dyn Fn(String) -> bool + Send + Sync>,
    ) -> io::Result<Option<Listener>> {
        let name = endpoint.0.clone();
        let mut server = match instance(&name, true) {
            Ok(server) => server,
            // Another Zmail serves it (pipes go when their process does,
            // so none is ever stale).
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
            Err(error) => return Err(error),
        };
        let task = runtime.spawn(async move {
            loop {
                if server.connect().await.is_err() {
                    // Start again on a fresh instance.
                    match instance(&name, false) {
                        Ok(fresh) => server = fresh,
                        Err(_) => return,
                    }
                    continue;
                }
                let Ok(next) = instance(&name, false) else {
                    return;
                };
                let connected = std::mem::replace(&mut server, next);
                tokio::spawn(serve(connected, open.clone()));
            }
        });
        Ok(Some(Listener { task }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A runtime, an endpoint in a fresh data directory, and what the
    /// "running app" received.
    struct Running {
        runtime: tokio::runtime::Runtime,
        endpoint: Endpoint,
        received: Arc<Mutex<Vec<String>>>,
        _dir: TempDir,
    }

    /// A temporary data directory, removed afterwards.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn data_dir() -> TempDir {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "zmail-single-instance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("the data directory is made");
        TempDir(dir)
    }

    fn running() -> Running {
        let dir = data_dir();
        let endpoint = Endpoint::for_data_dir(&dir.0).expect("there is an endpoint");
        Running {
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("the runtime starts"),
            endpoint,
            received: Arc::default(),
            _dir: dir,
        }
    }

    impl Running {
        fn listen(&self) -> io::Result<Option<Listener>> {
            let received = self.received.clone();
            listen(&self.endpoint, self.runtime.handle(), move |link| {
                received.lock().expect("not poisoned").push(link);
                true
            })
        }

        fn received(&self) -> Vec<String> {
            self.received.lock().expect("not poisoned").clone()
        }

        /// Sends `payload` as it is and returns the answer.
        fn send_raw(&self, payload: &[u8]) -> Vec<u8> {
            self.runtime.block_on(async {
                let mut stream = platform::connect(&self.endpoint)
                    .await
                    .expect("the running app answers");
                // The app may answer and close before reading it all.
                let _ = stream.write_all(payload).await;
                // Done sending, so a payload with no end of line is
                // answered now (a named pipe has no half-close: there the
                // app waits out its timeout).
                let _ = stream.shutdown().await;
                read_answer(&mut stream).await.unwrap_or_default()
            })
        }
    }

    const LINK: &str = "mailto:help@example.com?subject=Order%2042&body=Hello";

    #[test]
    fn a_second_launch_hands_its_link_to_the_first() {
        let app = running();
        let _listener = app.listen().expect("it listens").expect("first instance");
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::Delivered);
        assert_eq!(app.received(), [LINK]);
    }

    #[test]
    fn with_no_app_running_the_link_is_not_delivered() {
        let app = running();
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::NotDelivered);
        // Once the app that started instead listens, later links reach it.
        let _listener = app.listen().expect("it listens").expect("first instance");
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::Delivered);
    }

    #[test]
    fn a_second_app_does_not_take_over_a_running_one() {
        let app = running();
        let _first = app.listen().expect("it listens").expect("first instance");
        assert!(app.listen().expect("it checks").is_none());
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::Delivered);
        assert_eq!(app.received(), [LINK]);
    }

    #[test]
    fn a_link_the_app_cannot_take_is_not_delivered() {
        let app = running();
        let _listener = listen(&app.endpoint, app.runtime.handle(), |_| false)
            .expect("it listens")
            .expect("first instance");
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::NotDelivered);
    }

    #[test]
    fn anything_but_a_mailto_link_is_refused() {
        let app = running();
        let _listener = app.listen().expect("it listens").expect("first instance");
        let mut oversized = b"mailto:a@example.com?body=".to_vec();
        oversized.resize(MAX_LINK_BYTES + 10, b'a');
        oversized.push(b'\n');
        for payload in [
            b"https://example.com/\n".to_vec(),
            b"file:///etc/passwd\n".to_vec(),
            b"--help\n".to_vec(),
            b"mailto:a@example.com%0D%0ARCPT%20TO:<b@example.com>\n".to_vec(),
            // No end of line: not a whole link.
            b"mailto:a@example.com".to_vec(),
            b"mailto:a@example.com\xff\n".to_vec(),
            oversized,
        ] {
            assert_eq!(
                app.send_raw(&payload),
                REFUSED,
                "{:?}",
                String::from_utf8_lossy(&payload[..payload.len().min(40)])
            );
        }
        assert!(app.received().is_empty(), "nothing was opened");
        // The app still takes a good link after all that.
        assert_eq!(app.send_raw(format!("{LINK}\n").as_bytes()), ACCEPTED);
        assert_eq!(app.received(), [LINK]);
    }

    #[test]
    fn the_endpoint_follows_the_data_directory() {
        let one = data_dir();
        let two = data_dir();
        let first = Endpoint::for_data_dir(&one.0).expect("an endpoint");
        assert_eq!(Endpoint::for_data_dir(&one.0), Some(first.clone()));
        assert_ne!(Endpoint::for_data_dir(&two.0), Some(first));
    }

    #[cfg(unix)]
    #[test]
    fn a_socket_left_by_an_app_that_died_is_replaced() {
        let app = running();
        // A socket nobody listens on any more.
        drop(std::os::unix::net::UnixListener::bind(app.endpoint.path()).expect("it binds"));
        assert!(app.endpoint.path().exists());
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::NotDelivered);

        let _listener = app
            .listen()
            .expect("it listens")
            .expect("the stale socket is replaced");
        assert_eq!(hand_off(&app.endpoint, LINK), HandOff::Delivered);
        assert_eq!(app.received(), [LINK]);
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_is_private_and_goes_with_the_app() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let app = running();
        let listener = app.listen().expect("it listens").expect("first instance");
        let path = app.endpoint.path().to_path_buf();
        let metadata = std::fs::metadata(&path).expect("the socket exists");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let dir = std::fs::metadata(path.parent().expect("a folder")).expect("it exists");
        assert_eq!(dir.mode() & 0o077, 0, "only the user can reach the socket");
        drop(listener);
        assert!(!path.exists(), "the socket goes when the app does");
    }
}
