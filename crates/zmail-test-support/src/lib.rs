//! Test support utilities, synthetic datasets, and the Phase 2b mail harness.

use std::{
    fs,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde_json::json;

/// Parameters for a generated mailbox dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntheticMailboxSpec {
    /// Number of mailboxes to generate.
    pub mailbox_count: usize,
    /// Number of threads per mailbox.
    pub threads_per_mailbox: usize,
    /// Number of messages per thread.
    pub messages_per_thread: usize,
    /// Target average body size for later body generation work.
    pub average_body_bytes: usize,
}

impl SyntheticMailboxSpec {
    /// Returns the total message count represented by the spec.
    pub const fn total_messages(self) -> usize {
        self.mailbox_count * self.threads_per_mailbox * self.messages_per_thread
    }

    /// Produces deterministic mailbox names for the synthetic dataset.
    pub fn mailbox_names(self) -> Vec<String> {
        (0..self.mailbox_count)
            .map(|index| format!("Synthetic/{index:03}"))
            .collect()
    }
}

/// The default Phase 0 benchmark dataset shape.
pub const DEFAULT_SPEC: SyntheticMailboxSpec = SyntheticMailboxSpec {
    mailbox_count: 128,
    threads_per_mailbox: 2_000,
    messages_per_thread: 3,
    average_body_bytes: 8_192,
};

/// Docker Compose project name for the live mail harness.
pub const MAIL_HARNESS_PROJECT: &str = "zmail-phase2b-mail";
/// Hostname used by the local mail harness.
pub const MAIL_HARNESS_HOST: &str = "127.0.0.1";
/// Local IMAP endpoint for the live harness.
pub const MAIL_HARNESS_IMAP_ENDPOINT: &str = "imap://127.0.0.1:3143";
/// Local SMTP port for the live harness.
pub const MAIL_HARNESS_SMTP_PORT: u16 = 3025;
/// Local JMAP base URL for the live harness.
pub const MAIL_HARNESS_JMAP_ENDPOINT: &str = "http://127.0.0.1:4080";
/// Local James WebAdmin base URL for provisioning the JMAP harness.
pub const MAIL_HARNESS_JMAP_ADMIN_ENDPOINT: &str = "http://127.0.0.1:4081";
/// Local SMTP port for the JMAP mail server.
pub const MAIL_HARNESS_JMAP_SMTP_PORT: u16 = 4025;
/// Mailbox account seeded into the live harness.
pub const MAIL_HARNESS_RECIPIENT: &str = "phase2@localhost";
/// IMAP login for the seeded mailbox account.
pub const MAIL_HARNESS_RECIPIENT_LOGIN: &str = "phase2";
/// Password for the seeded mailbox account.
pub const MAIL_HARNESS_RECIPIENT_PASSWORD: &str = "phase2-password";
/// SMTP sender account seeded into the live harness.
pub const MAIL_HARNESS_SENDER: &str = "sender@localhost";
/// SMTP AUTH login for the seeded sender account.
pub const MAIL_HARNESS_SENDER_LOGIN: &str = "sender";
/// Password for the seeded SMTP sender.
pub const MAIL_HARNESS_SENDER_PASSWORD: &str = "sender-password";
/// Subject of the mail preloaded into the Phase 2b harness.
pub const MAIL_HARNESS_PRELOADED_SUBJECT: &str = "Preloaded Phase 2b message";
/// JMAP account used by the live harness.
pub const MAIL_HARNESS_JMAP_ACCOUNT: &str = "jmap@localhost";
/// Password for the live harness JMAP account.
pub const MAIL_HARNESS_JMAP_PASSWORD: &str = "jmap-password";

/// Serializes live-harness tests within one test binary.
///
/// Every live harness test owns the whole Docker Compose project for its
/// duration: [`DockerMailHarness::start`] tears any previous stack down and
/// [`DockerMailHarness::down`] removes it on exit. Tests therefore must not
/// run concurrently, so each one grabs this lock before starting the harness.
pub fn live_harness_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Controller for the Docker-backed IMAP/SMTP test harness.
#[derive(Debug)]
pub struct DockerMailHarness {
    compose_file: PathBuf,
}

impl DockerMailHarness {
    /// Starts the mail harness in a known-good state and waits for its ports to accept connections.
    pub fn start() -> Result<Self> {
        let compose_file = repo_root().join("docker/mail-harness/docker-compose.yml");
        let harness = Self { compose_file };

        harness.ensure_james_keystore()?;
        let _ = harness.compose(["down", "-v", "--remove-orphans"]);
        harness.compose(["up", "-d", "--remove-orphans"])?;
        harness.wait_until_ready(Duration::from_secs(30))?;
        harness.provision_jmap_accounts(Duration::from_secs(30))?;

        Ok(harness)
    }

    /// Stops the harness and removes any transient mail state.
    pub fn down(&self) -> Result<()> {
        self.compose(["down", "-v", "--remove-orphans"])
    }

    fn wait_until_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let smtp = SocketAddr::from(([127, 0, 0, 1], MAIL_HARNESS_SMTP_PORT));
        let imap = SocketAddr::from(([127, 0, 0, 1], 3143));
        let jmap_smtp = SocketAddr::from(([127, 0, 0, 1], MAIL_HARNESS_JMAP_SMTP_PORT));
        let jmap_http = SocketAddr::from(([127, 0, 0, 1], 4080));
        let jmap_admin = SocketAddr::from(([127, 0, 0, 1], 4081));

        while Instant::now() < deadline {
            if port_is_open(smtp)
                && port_is_open(imap)
                && port_is_open(jmap_smtp)
                && port_is_open(jmap_http)
                && port_is_open(jmap_admin)
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }

        bail!(
            "mail harness did not become ready within {} seconds",
            timeout.as_secs()
        )
    }

    fn provision_jmap_accounts(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("failed to construct the James WebAdmin client")?;
        let domain_url = format!("{MAIL_HARNESS_JMAP_ADMIN_ENDPOINT}/domains/localhost");
        let primary_url =
            format!("{MAIL_HARNESS_JMAP_ADMIN_ENDPOINT}/users/{MAIL_HARNESS_JMAP_ACCOUNT}");
        let sender_url = format!("{MAIL_HARNESS_JMAP_ADMIN_ENDPOINT}/users/{MAIL_HARNESS_SENDER}");

        while Instant::now() < deadline {
            let domain = client.put(&domain_url).send();
            let primary = client
                .put(&primary_url)
                .json(&json!({ "password": MAIL_HARNESS_JMAP_PASSWORD }))
                .send();
            let sender = client
                .put(&sender_url)
                .json(&json!({ "password": MAIL_HARNESS_SENDER_PASSWORD }))
                .send();

            if response_is_success(&domain)
                && response_is_success(&primary)
                && response_is_success(&sender)
            {
                return Ok(());
            }

            thread::sleep(Duration::from_millis(500));
        }

        bail!(
            "failed to provision James JMAP accounts within {} seconds",
            timeout.as_secs()
        )
    }

    fn ensure_james_keystore(&self) -> Result<()> {
        let keystore = repo_root().join("docker/mail-harness/james/conf/keystore");
        if keystore.exists() {
            return Ok(());
        }

        let parent = keystore
            .parent()
            .context("James keystore path should have a parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        let output = Command::new("keytool")
            .args([
                "-genkeypair",
                "-alias",
                "james",
                "-keyalg",
                "RSA",
                "-keysize",
                "2048",
                "-storetype",
                "PKCS12",
                "-keystore",
            ])
            .arg(&keystore)
            .args([
                "-storepass",
                "james72laBalle",
                "-keypass",
                "james72laBalle",
                "-dname",
                "CN=localhost, OU=Zmail, O=Zmail, L=Test, ST=Test, C=US",
                "-validity",
                "3650",
                "-noprompt",
            ])
            .output()
            .context("failed to invoke keytool to create the James harness keystore")?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "failed to generate the James harness keystore: {}",
                stderr.trim()
            )
        }
    }

    fn compose<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let output = self.compose_output(args)?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("docker compose failed: {}", stderr.trim());
        }
    }

    fn compose_output<I, S>(&self, args: I) -> Result<std::process::Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Command::new("docker")
            .arg("compose")
            .arg("-p")
            .arg(MAIL_HARNESS_PROJECT)
            .arg("-f")
            .arg(&self.compose_file)
            .args(args.into_iter().map(|arg| arg.as_ref().to_owned()))
            .output()
            .with_context(|| {
                format!(
                    "failed to invoke docker compose for {}",
                    self.compose_file.display()
                )
            })
    }
}

impl Drop for DockerMailHarness {
    fn drop(&mut self) {
        let _ = self.down();
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root should be discoverable from zmail-test-support")
}

fn port_is_open(address: SocketAddr) -> bool {
    TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
}

fn response_is_success(
    result: &std::result::Result<reqwest::blocking::Response, reqwest::Error>,
) -> bool {
    result
        .as_ref()
        .map(|response| response.status().is_success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_SPEC, MAIL_HARNESS_IMAP_ENDPOINT, MAIL_HARNESS_JMAP_ACCOUNT,
        MAIL_HARNESS_JMAP_ADMIN_ENDPOINT, MAIL_HARNESS_JMAP_ENDPOINT,
        MAIL_HARNESS_PRELOADED_SUBJECT, SyntheticMailboxSpec,
    };

    #[test]
    fn computes_total_messages() {
        let spec = SyntheticMailboxSpec {
            mailbox_count: 10,
            threads_per_mailbox: 500,
            messages_per_thread: 4,
            average_body_bytes: 1_024,
        };

        assert_eq!(spec.total_messages(), 20_000);
    }

    #[test]
    fn produces_deterministic_mailbox_names() {
        let names = DEFAULT_SPEC.mailbox_names();

        assert_eq!(names.first().map(String::as_str), Some("Synthetic/000"));
        assert_eq!(names.get(127).map(String::as_str), Some("Synthetic/127"));
    }

    #[test]
    fn exposes_the_seeded_mail_harness_contract() {
        assert_eq!(MAIL_HARNESS_IMAP_ENDPOINT, "imap://127.0.0.1:3143");
        assert_eq!(MAIL_HARNESS_JMAP_ENDPOINT, "http://127.0.0.1:4080");
        assert_eq!(MAIL_HARNESS_JMAP_ADMIN_ENDPOINT, "http://127.0.0.1:4081");
        assert_eq!(MAIL_HARNESS_JMAP_ACCOUNT, "jmap@localhost");
        assert_eq!(MAIL_HARNESS_PRELOADED_SUBJECT, "Preloaded Phase 2b message");
    }
}
