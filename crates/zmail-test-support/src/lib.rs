//! Test support utilities, synthetic datasets, the Phase 2b mail harness and
//! the Proton API test double.

mod proton_harness;

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
use zmail_core::{
    AccountSyncSnapshot, MailboxSnapshot, MessageAttachment, MessageSnapshot, ThreadSnapshot,
};

pub use proton_harness::{
    PROTON_HARNESS_ALIAS_ADDRESS, PROTON_HARNESS_PASSWORD, PROTON_HARNESS_PRIMARY_ADDRESS,
    PROTON_HARNESS_URL, PROTON_HARNESS_USERNAME, ProtonHarness, ProtonHarnessAddress,
    ProtonHarnessLabel, ProtonHarnessLabelKind, ProtonHarnessMessage, ProtonHarnessState,
};

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

impl SyntheticMailboxSpec {
    /// Generates the dataset as one account snapshot, deterministically
    /// from `seed`.
    ///
    /// Every thread gets a unique `T<n>` tag in its subject; messages draw
    /// senders, recipients, subject and body words from small fixed
    /// vocabularies, so text queries hit realistic fractions of the
    /// mailbox. Timestamps spread back two years from 2026-10-01 (UTC).
    /// Roughly 20% of messages are unread, 5% flagged and 8% carry an
    /// attachment. One message in `hydrated_every` has its body stored
    /// (`0` stores none, like a fresh lazy IMAP sync); the rest carry only
    /// a preview, as headers-only messages do.
    pub fn snapshot(self, seed: u64, hydrated_every: usize) -> AccountSyncSnapshot {
        let mut random = SyntheticRandom(seed ^ 0x9e37_79b9_7f4a_7c15);
        let base = 1_790_812_800_i64; // 2026-10-01T00:00:00Z
        let span = 2 * 365 * 24 * 3600_i64;
        let mut ordinal = 0usize;
        let mailboxes = self
            .mailbox_names()
            .into_iter()
            .enumerate()
            .map(|(mailbox_index, name)| {
                let threads = (0..self.threads_per_mailbox)
                    .map(|_| {
                        let thread_number = ordinal;
                        let topic = random.words(SUBJECT_WORDS, 3);
                        let subject = format!("{topic} T{thread_number}");
                        let start = base - random.below(span as u64) as i64;
                        let messages = (0..self.messages_per_thread)
                            .map(|position| {
                                ordinal += 1;
                                let (first, last, domain) = random.person();
                                let (to_first, to_last, to_domain) = random.person();
                                let at = start + position as i64 * 3_600;
                                let received_at = chrono::DateTime::from_timestamp(at, 0)
                                    .unwrap_or_default()
                                    .to_rfc3339();
                                let body = random.body(self.average_body_bytes);
                                let preview = body.chars().take(120).collect::<String>();
                                let hydrated =
                                    hydrated_every > 0 && ordinal.is_multiple_of(hydrated_every);
                                let attachments = if random.below(100) < 8 {
                                    vec![MessageAttachment {
                                        filename: "report.pdf".to_owned(),
                                        content_type: "application/pdf".to_owned(),
                                        size_bytes: 1_024,
                                        storage_path: format!("synthetic/{ordinal}.pdf"),
                                    }]
                                } else {
                                    Vec::new()
                                };
                                MessageSnapshot {
                                    remote_id: format!("{mailbox_index}-{ordinal}"),
                                    identity_email: "synthetic@zmail.test".to_owned(),
                                    sender: format!(
                                        "{first} {last} <{}.{}@{domain}>",
                                        first.to_lowercase(),
                                        last.to_lowercase()
                                    ),
                                    reply_to: None,
                                    recipients: format!("{to_first}.{to_last}@{to_domain}")
                                        .to_lowercase(),
                                    subject: subject.clone(),
                                    body_preview: preview,
                                    body_text: hydrated.then_some(body),
                                    sanitized_html: None,
                                    has_remote_content: false,
                                    is_read: random.below(100) >= 20,
                                    is_flagged: random.below(100) < 5,
                                    received_at,
                                    attachments,
                                }
                            })
                            .collect::<Vec<_>>();
                        ThreadSnapshot {
                            remote_thread_id: format!("thread-{thread_number}"),
                            subject,
                            participants: zmail_core::participants::thread_participants(&messages),
                            preview: messages
                                .last()
                                .map(|message| message.body_preview.clone())
                                .unwrap_or_default(),
                            is_flagged: messages.iter().any(|message| message.is_flagged),
                            has_attachments: messages
                                .iter()
                                .any(|message| !message.attachments.is_empty()),
                            unread_count: messages
                                .iter()
                                .filter(|message| !message.is_read)
                                .count(),
                            latest_message_at: messages
                                .last()
                                .map(|message| message.received_at.clone())
                                .unwrap_or_default(),
                            messages,
                        }
                    })
                    .collect();
                MailboxSnapshot {
                    role: (mailbox_index == 0).then(|| "inbox".to_owned()),
                    name,
                    threads,
                    ..Default::default()
                }
            })
            .collect();
        AccountSyncSnapshot {
            checkpoint: format!("synthetic-{seed}"),
            detail: "synthetic mailbox".to_owned(),
            identities: None,
            mailboxes,
        }
    }
}

/// Subject vocabulary of the synthetic dataset.
pub const SUBJECT_WORDS: &[&str] = &[
    "budget", "review", "launch", "incident", "invoice", "meeting", "travel", "report", "deploy",
    "holiday", "contract", "hiring", "roadmap", "security", "outage", "release",
];
/// Body vocabulary of the synthetic dataset (a superset of the subject
/// words).
pub const BODY_WORDS: &[&str] = &[
    "budget",
    "review",
    "launch",
    "incident",
    "invoice",
    "meeting",
    "travel",
    "report",
    "deploy",
    "holiday",
    "contract",
    "hiring",
    "roadmap",
    "security",
    "outage",
    "release",
    "please",
    "attached",
    "tomorrow",
    "numbers",
    "agenda",
    "customer",
    "migration",
    "schedule",
    "approval",
    "draft",
    "feedback",
    "quarter",
    "pipeline",
    "latency",
    "vendor",
    "renewal",
    "kittiwake",
];
const FIRST_NAMES: &[&str] = &[
    "Alice", "Bob", "Carol", "Dmitri", "Erin", "Farah", "Gus", "Hana", "Ivan", "Jules", "Kai",
    "Lena", "Mateo", "Nia", "Oscar", "Priya",
];
const LAST_NAMES: &[&str] = &[
    "Smith", "Jones", "Okafor", "Novak", "Tanaka", "Silva", "Murphy", "Kowalski", "Haddad",
    "Larsen", "Moreau", "Quinn",
];
const DOMAINS: &[&str] = &["example.com", "partner.test", "vendor.test", "zmail.test"];

/// A small deterministic generator (64-bit LCG), so datasets are stable
/// across runs and platforms without a `rand` dependency.
struct SyntheticRandom(u64);

impl SyntheticRandom {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }

    fn words(&mut self, vocabulary: &[&str], count: usize) -> String {
        (0..count)
            .map(|_| self.pick(vocabulary))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn person(&mut self) -> (&'static str, &'static str, &'static str) {
        (
            self.pick(FIRST_NAMES),
            self.pick(LAST_NAMES),
            self.pick(DOMAINS),
        )
    }

    fn body(&mut self, average_bytes: usize) -> String {
        let target = average_bytes / 2 + self.below(average_bytes.max(1) as u64) as usize;
        let mut body = String::with_capacity(target + 16);
        while body.len() < target {
            if !body.is_empty() {
                body.push(' ');
            }
            body.push_str(self.pick(BODY_WORDS));
        }
        body
    }
}

/// Subject of [`reader_stress_threads`]' conversation of 100 unread
/// messages.
pub const STRESS_LONG_SUBJECT: &str = "Stress: long conversation";
/// Subject of [`reader_stress_threads`]' message with a 1 MB HTML body.
pub const STRESS_HTML_SUBJECT: &str = "Stress: large HTML body";
/// Subject of [`reader_stress_threads`]' message with 60 attachments.
pub const STRESS_ATTACHMENTS_SUBJECT: &str = "Stress: many attachments";

/// Conversations that are heavy to open in the reader, for the reader
/// benchmarks (#122): 100 unread messages of about 2 KB each, one message
/// whose only body is 1 MB of sanitised HTML, and one message with 60
/// attachments. All three are dated 2026-10-02, after every message of
/// [`SyntheticMailboxSpec::snapshot`], so they list first.
pub fn reader_stress_threads() -> Vec<ThreadSnapshot> {
    let long = (0..100)
        .map(|index| {
            stress_message(
                &format!("long-{index}"),
                STRESS_LONG_SUBJECT,
                index,
                Some(stress_text_body(index, 2_000)),
            )
        })
        .collect();
    let mut html = stress_message("html", STRESS_HTML_SUBJECT, 200, None);
    html.sanitized_html = Some(stress_html(1_000_000));
    html.body_preview = "Quarterly report".to_owned();
    let mut attachments = stress_message(
        "attachments",
        STRESS_ATTACHMENTS_SUBJECT,
        300,
        Some(stress_text_body(0, 800)),
    );
    attachments.attachments = (0..60)
        .map(|index| MessageAttachment {
            filename: format!("scan-{index:02}.pdf"),
            content_type: "application/pdf".to_owned(),
            size_bytes: 250_000 + index * 1_000,
            storage_path: format!("stress/{index}.pdf"),
        })
        .collect();
    vec![
        stress_thread(STRESS_LONG_SUBJECT, long),
        stress_thread(STRESS_HTML_SUBJECT, vec![html]),
        stress_thread(STRESS_ATTACHMENTS_SUBJECT, vec![attachments]),
    ]
}

fn stress_message(id: &str, subject: &str, minute: usize, body: Option<String>) -> MessageSnapshot {
    MessageSnapshot {
        remote_id: format!("stress-{id}"),
        identity_email: "synthetic@zmail.test".to_owned(),
        sender: format!("Sender {0} <sender{0}@example.com>", minute % 7),
        reply_to: None,
        recipients: "synthetic@zmail.test".to_owned(),
        subject: subject.to_owned(),
        body_preview: body
            .as_deref()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect(),
        body_text: body,
        sanitized_html: None,
        has_remote_content: false,
        is_read: false,
        is_flagged: false,
        received_at: format!("2026-10-02T{:02}:{:02}:00+00:00", minute / 60, minute % 60),
        attachments: Vec::new(),
    }
}

fn stress_thread(subject: &str, messages: Vec<MessageSnapshot>) -> ThreadSnapshot {
    ThreadSnapshot {
        remote_thread_id: format!("stress-{subject}"),
        subject: subject.to_owned(),
        participants: zmail_core::participants::thread_participants(&messages),
        preview: messages
            .last()
            .map(|message| message.body_preview.clone())
            .unwrap_or_default(),
        is_flagged: false,
        has_attachments: messages
            .iter()
            .any(|message| !message.attachments.is_empty()),
        unread_count: messages.len(),
        latest_message_at: messages
            .last()
            .map(|message| message.received_at.clone())
            .unwrap_or_default(),
        messages,
    }
}

/// A plain-text reply of about `bytes`: paragraphs with a link, and a
/// quoted tail that folds.
fn stress_text_body(index: usize, bytes: usize) -> String {
    let paragraph = "Thanks for the numbers. The budget review moved to Thursday, \
        and the launch checklist is at https://example.com/launch for anyone \
        who has not seen it yet. Please add your notes before the meeting.";
    let mut body = format!("Hello {index},\n\n");
    while body.len() < bytes {
        body.push_str(paragraph);
        body.push_str("\n\n");
    }
    body.push_str("On Monday, someone wrote:\n> the earlier message\n> quoted here\n");
    body
}

/// About `bytes` of sanitised HTML, as a report or newsletter has:
/// headings, paragraphs with links, and table rows.
fn stress_html(bytes: usize) -> String {
    let mut html = String::from("<html><body><h1>Quarterly report</h1>");
    let mut row = 0;
    while html.len() < bytes {
        html.push_str(&format!(
            "<h2>Section {row}</h2><p>The <b>budget</b> for item {row} is on \
             <a href=\"https://example.com/items/{row}\">its page</a>, with the \
             launch notes and the vendor's renewal terms.</p><table><tr><td>Item \
             {row}</td><td>{}</td><td>approved</td></tr></table>",
            row * 17
        ));
        row += 1;
    }
    html.push_str("</body></html>");
    html
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
/// Local IMAP endpoint for the CONDSTORE-capable Dovecot harness.
pub const MAIL_HARNESS_DOVECOT_ENDPOINT: &str = "imap://127.0.0.1:3243";
/// IMAP login for the Dovecot harness account.
pub const MAIL_HARNESS_DOVECOT_LOGIN: &str = "mailuser";
/// Password for the Dovecot harness account.
pub const MAIL_HARNESS_DOVECOT_PASSWORD: &str = "mailuser-password";
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
        // James is a JVM server; on a busy CI runner it can take well over a
        // minute after its ports open before WebAdmin accepts writes.
        harness.provision_jmap_accounts(Duration::from_secs(240))?;

        Ok(harness)
    }

    /// Stops the harness and removes any transient mail state.
    pub fn down(&self) -> Result<()> {
        self.compose(["down", "-v", "--remove-orphans"])
    }

    /// Returns the last `tail` log lines of one harness service, for
    /// diagnosing a failed live test.
    pub fn logs(&self, service: &str, tail: usize) -> String {
        match self.compose_output(["logs", "--no-color", "--tail", &tail.to_string(), service]) {
            Ok(output) => format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => format!("could not read the {service} logs: {error:#}"),
        }
    }

    fn wait_until_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let smtp = SocketAddr::from(([127, 0, 0, 1], MAIL_HARNESS_SMTP_PORT));
        let dovecot = SocketAddr::from(([127, 0, 0, 1], 3243));
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
                && port_is_open(dovecot)
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

        let mut last_failure = String::from("no attempt finished");
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
                && user_is_provisioned(&primary)
                && user_is_provisioned(&sender)
            {
                return Ok(());
            }

            last_failure = [
                ("domain", &domain),
                ("primary", &primary),
                ("sender", &sender),
            ]
            .iter()
            .map(|(what, result)| format!("{what}: {}", describe_response(result)))
            .collect::<Vec<_>>()
            .join(", ");
            thread::sleep(Duration::from_millis(500));
        }

        bail!(
            "failed to provision James JMAP accounts within {} seconds (last attempt: {last_failure})",
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

fn describe_response(
    result: &std::result::Result<reqwest::blocking::Response, reqwest::Error>,
) -> String {
    match result {
        Ok(response) => response.status().to_string(),
        Err(error) => error.to_string(),
    }
}

/// James answers 409 Conflict to a PUT for a user that already exists. The
/// stack is wiped before every start, so an existing user was created by an
/// earlier round of this loop, with the same password; retrying would only
/// ever see 409 again.
fn user_is_provisioned(
    result: &std::result::Result<reqwest::blocking::Response, reqwest::Error>,
) -> bool {
    response_is_success(result)
        || result
            .as_ref()
            .is_ok_and(|response| response.status() == reqwest::StatusCode::CONFLICT)
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
