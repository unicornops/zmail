//! The Proton API test double: go-proton-api's fake server, run by
//! `scripts/proton-harness.sh` (see `docker/proton-harness/README.md`).
//!
//! The script is the single source of truth for building, starting and
//! seeding the server; [`ProtonHarness`] drives it and reads back the
//! `state.json` it writes, so a test gets the base URL, the throwaway CA to
//! trust and the seeded account without knowing how they were made.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use reqwest::{
    Certificate,
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::Value;

use crate::repo_root;

/// Base URL of the harness API (TLS, signed by the per-run harness CA).
pub const PROTON_HARNESS_URL: &str = "https://127.0.0.1:18443";
/// Login name of the seeded Proton account.
pub const PROTON_HARNESS_USERNAME: &str = "zmail";
/// Password of the seeded Proton account (single-password mode).
pub const PROTON_HARNESS_PASSWORD: &str = "zmail-proton-password";
/// Primary address of the seeded account.
pub const PROTON_HARNESS_PRIMARY_ADDRESS: &str = "zmail@proton.local";
/// Second address of the seeded account.
pub const PROTON_HARNESS_ALIAS_ADDRESS: &str = "zmail.alias@proton.local";

/// Times `ProtonHarness::start` runs the script before giving up. The script
/// already retries the image build and waits for readiness; this covers a
/// start that fails outright, such as a port still held by a dying server.
const START_ATTEMPTS: usize = 2;

/// One address of the seeded account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonHarnessAddress {
    /// The address, such as `zmail@proton.local`.
    pub email: String,
    /// The server's address ID.
    pub id: String,
}

/// Whether a seeded label is a folder (exclusive) or a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtonHarnessLabelKind {
    /// An exclusive label: a message is in at most one folder.
    Folder,
    /// A non-exclusive label.
    Label,
}

/// One label or folder of the seeded account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonHarnessLabel {
    /// Display name.
    pub name: String,
    /// Folder or label.
    pub kind: ProtonHarnessLabelKind,
    /// The server's label ID.
    pub id: String,
}

/// One message imported from `docker/proton-harness/messages`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonHarnessMessage {
    /// The fixture file, such as `welcome.eml`.
    pub file: String,
    /// The server's message ID.
    pub id: String,
    /// The fixture's subject, as listed in the manifest.
    pub subject: String,
}

/// Everything the script recorded about a running, seeded harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonHarnessState {
    /// Base URL of the API.
    pub base_url: String,
    /// The throwaway CA that signed the API's certificate.
    pub ca_pem_path: PathBuf,
    /// The `x-pm-appversion` value the script used.
    pub app_version: String,
    /// Login name of the seeded account.
    pub username: String,
    /// Password of the seeded account.
    pub password: String,
    /// The server's user ID for the seeded account.
    pub user_id: String,
    /// The account's addresses, primary first.
    pub addresses: Vec<ProtonHarnessAddress>,
    /// The seeded folders and labels.
    pub labels: Vec<ProtonHarnessLabel>,
    /// The imported messages, in manifest order.
    pub messages: Vec<ProtonHarnessMessage>,
}

impl ProtonHarnessState {
    /// Parses the `state.json` written by `scripts/proton-harness.sh seed`.
    pub fn from_json(json: &str) -> Result<Self> {
        let value: Value =
            serde_json::from_str(json).context("proton harness state is not valid JSON")?;

        let addresses = array(&value, "addresses")?
            .iter()
            .map(|address| {
                Ok(ProtonHarnessAddress {
                    email: string(address, "email")?,
                    id: string(address, "id")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let labels = array(&value, "labels")?
            .iter()
            .map(|label| {
                let kind = match string(label, "kind")?.as_str() {
                    "folder" => ProtonHarnessLabelKind::Folder,
                    "label" => ProtonHarnessLabelKind::Label,
                    other => bail!("unknown proton harness label kind {other:?}"),
                };
                Ok(ProtonHarnessLabel {
                    name: string(label, "name")?,
                    kind,
                    id: string(label, "id")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let messages = array(&value, "messages")?
            .iter()
            .map(|message| {
                Ok(ProtonHarnessMessage {
                    file: string(message, "file")?,
                    id: string(message, "id")?,
                    subject: string(message, "subject")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            base_url: string(&value, "base_url")?,
            ca_pem_path: PathBuf::from(string(&value, "ca_pem")?),
            app_version: string(&value, "app_version")?,
            username: string(&value, "username")?,
            password: string(&value, "password")?,
            user_id: string(&value, "user_id")?,
            addresses,
            labels,
            messages,
        })
    }
}

/// Controller for the Proton API test double.
///
/// Like [`crate::DockerMailHarness`], one harness owns the server for its
/// lifetime: [`ProtonHarness::start`] replaces any running one and dropping
/// the harness stops it, so tests hold [`crate::live_harness_lock`] while
/// they use it. `PROTON_HARNESS_MODE=native` (read by the script) runs the
/// server with a local Go toolchain instead of Docker.
#[derive(Debug)]
pub struct ProtonHarness {
    state_dir: PathBuf,
    state: ProtonHarnessState,
}

impl ProtonHarness {
    /// Starts a fresh, seeded harness and waits until its API answers.
    pub fn start() -> Result<Self> {
        let state_dir = state_dir();
        let mut failures = Vec::new();
        for attempt in 1..=START_ATTEMPTS {
            match run_script(&state_dir, "reset") {
                Ok(()) => {
                    let path = state_dir.join("state.json");
                    let json = fs::read_to_string(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?;
                    let state = ProtonHarnessState::from_json(&json)?;
                    return Ok(Self { state_dir, state });
                }
                Err(error) => failures.push(format!("attempt {attempt}: {error:#}")),
            }
        }
        let _ = run_script(&state_dir, "down");
        bail!(
            "the Proton harness did not start after {START_ATTEMPTS} attempts:\n{}",
            failures.join("\n")
        )
    }

    /// Stops the harness and removes its state, including the CA.
    pub fn down(&self) -> Result<()> {
        run_script(&self.state_dir, "down")
    }

    /// What the harness was seeded with.
    pub fn state(&self) -> &ProtonHarnessState {
        &self.state
    }

    /// Base URL of the harness API, `https://127.0.0.1:18443`.
    pub fn base_url(&self) -> &str {
        &self.state.base_url
    }

    /// Path of the throwaway CA that signed the harness's certificate.
    pub fn ca_pem_path(&self) -> &Path {
        &self.state.ca_pem_path
    }

    /// The throwaway CA, PEM encoded.
    pub fn ca_pem(&self) -> Result<Vec<u8>> {
        fs::read(&self.state.ca_pem_path)
            .with_context(|| format!("failed to read {}", self.state.ca_pem_path.display()))
    }

    /// Login name of the seeded account.
    pub fn username(&self) -> &str {
        &self.state.username
    }

    /// Password of the seeded account.
    pub fn password(&self) -> &str {
        &self.state.password
    }

    /// A blocking HTTP client for raw API calls: it trusts only the harness
    /// CA and sends the `x-pm-appversion` header the API requires.
    pub fn http_client(&self) -> Result<Client> {
        let ca = Certificate::from_pem(&self.ca_pem()?)
            .context("the proton harness CA is not a PEM certificate")?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-pm-appversion",
            HeaderValue::from_str(&self.state.app_version)
                .context("the proton harness app version is not a header value")?,
        );
        Client::builder()
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .default_headers(headers)
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .context("failed to construct the proton harness HTTP client")
    }

    /// The server log, for diagnosing a failed live test.
    pub fn logs(&self) -> String {
        match script_output(&self.state_dir, "logs") {
            Ok(output) => format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => format!("could not read the proton harness logs: {error:#}"),
        }
    }
}

impl Drop for ProtonHarness {
    fn drop(&mut self) {
        let _ = self.down();
    }
}

fn state_dir() -> PathBuf {
    env::var_os("PROTON_HARNESS_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo_root().join("target/proton-harness"))
}

fn run_script(state_dir: &Path, command: &str) -> Result<()> {
    let output = script_output(state_dir, command)?;
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "scripts/proton-harness.sh {command} failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn script_output(state_dir: &Path, command: &str) -> Result<std::process::Output> {
    let script = repo_root().join("scripts/proton-harness.sh");
    Command::new("bash")
        .arg(&script)
        .arg(command)
        .env("PROTON_HARNESS_STATE_DIR", state_dir)
        .output()
        .with_context(|| format!("failed to run {} {command}", script.display()))
}

fn string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("proton harness state has no string {field:?}"))
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>> {
    value
        .get(field)
        .and_then(Value::as_array)
        .with_context(|| format!("proton harness state has no array {field:?}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        PROTON_HARNESS_ALIAS_ADDRESS, PROTON_HARNESS_PASSWORD, PROTON_HARNESS_PRIMARY_ADDRESS,
        PROTON_HARNESS_URL, PROTON_HARNESS_USERNAME, ProtonHarnessLabelKind, ProtonHarnessState,
    };

    #[test]
    fn parses_the_state_the_harness_script_writes() {
        let json = r#"{
          "base_url": "https://127.0.0.1:18443",
          "ca_pem": "/repo/target/proton-harness/ca.pem",
          "app_version": "linux-other@0.0.0-harness",
          "username": "zmail",
          "password": "zmail-proton-password",
          "user_id": "user-1",
          "addresses": [
            {"email": "zmail@proton.local", "id": "addr-1"},
            {"email": "zmail.alias@proton.local", "id": "addr-2"}
          ],
          "labels": [
            {"name": "Work", "kind": "folder", "id": "label-1"},
            {"name": "Follow up", "kind": "label", "id": "label-2"}
          ],
          "messages": [
            {"file": "welcome.eml", "id": "msg-1", "subject": "Welcome to the harness"}
          ]
        }"#;

        let state = ProtonHarnessState::from_json(json).expect("state parses");

        assert_eq!(state.base_url, PROTON_HARNESS_URL);
        assert_eq!(
            state.ca_pem_path,
            PathBuf::from("/repo/target/proton-harness/ca.pem")
        );
        assert_eq!(state.username, PROTON_HARNESS_USERNAME);
        assert_eq!(state.password, PROTON_HARNESS_PASSWORD);
        assert_eq!(state.addresses[0].email, PROTON_HARNESS_PRIMARY_ADDRESS);
        assert_eq!(state.addresses[1].email, PROTON_HARNESS_ALIAS_ADDRESS);
        assert_eq!(state.labels[0].kind, ProtonHarnessLabelKind::Folder);
        assert_eq!(state.labels[1].kind, ProtonHarnessLabelKind::Label);
        assert_eq!(state.labels[1].id, "label-2");
        assert_eq!(state.messages[0].id, "msg-1");
        assert_eq!(state.messages[0].subject, "Welcome to the harness");
    }

    #[test]
    fn rejects_state_with_a_missing_field() {
        let error = ProtonHarnessState::from_json(r#"{"base_url": "https://127.0.0.1:18443"}"#)
            .expect_err("incomplete state is rejected");

        assert!(format!("{error:#}").contains("addresses"), "{error:#}");
    }

    #[test]
    fn matches_the_script_contract() {
        let script = include_str!("../../../scripts/proton-harness.sh");

        assert!(script.contains("API_PORT=18443"));
        assert!(script.contains(&format!("USERNAME=\"{PROTON_HARNESS_USERNAME}\"")));
        assert!(script.contains(&format!("PASSWORD=\"{PROTON_HARNESS_PASSWORD}\"")));
        assert!(script.contains(&format!("ALIAS_ADDRESS=\"{PROTON_HARNESS_ALIAS_ADDRESS}\"")));
    }
}
