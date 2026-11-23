# Development

Building Zmail from source, testing it, and where the design documents are. Most people want [Installing Zmail](./install.md) instead. Contributors (human and AI) should also read [`AGENTS.md`](../AGENTS.md), which has the crate map, the full verification commands and the repository's rules.

## Workspace

- [`PLAN.md`](../PLAN.md): master delivery plan
- [`docs/upstream/zed.md`](./upstream/zed.md): Zed and GPUI upstream pin record (GPUI is normally pinned to a Zed stable release tag and bumped by a weekly workflow; for now it is pinned to Zed PR #62379 for the web renderer)
- [`docs/upstream/proton.md`](./upstream/proton.md): Proton Mail Bridge and go-proton-api upstream pins and ported files
- [`docs/adr/`](./adr/): architecture decision records:
  - [ADR 0001](./adr/0001-jmap-client-strategy.md): JMAP implementation decision
  - [ADR 0002](./adr/0002-message-encryption-protocol.md): message encryption protocol
  - [ADR 0003](./adr/0003-gpui-zed-stable-pin.md): GPUI pinned to Zed stable release tags
  - [ADR 0004](./adr/0004-proton-native.md): native Proton Mail
  - [ADR 0005](./adr/0005-store-encryption.md): encryption of the local store at rest
  - [ADR 0006](./adr/0006-html-email-renderer.md): rendering HTML mail safely
  - [ADR 0007](./adr/0007-terminal-ui.md): the terminal UI
- [`docs/phase5-hardening.md`](./phase5-hardening.md): accessibility, performance, crash policy, packaging, and release process
- [`docs/performance.md`](./performance.md): performance budgets, how they are measured, and the latest numbers; [`benches/README.md`](../benches/README.md) has the store benchmarks
- [`docs/oauth2-live-verification.md`](./oauth2-live-verification.md): checking OAuth2 sign-in against real providers by hand
- [`docs/site.md`](./site.md): the documentation site: building it locally, how it is deployed, and adding a page (new pages go in [`docs/SUMMARY.md`](./SUMMARY.md))
- [`fuzz/README.md`](../fuzz/README.md): fuzzing the mail parsing
- [`themes/phase0-dark.json`](../themes/phase0-dark.json): sample unchanged Zed-format theme file

## Building from source

Format and verify the workspace:

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
```

Run the stricter CI-equivalent verification locally:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo deny check licenses advisories bans sources
cargo audit
```

Launch the desktop client:

```bash
cargo run -p zmail-app
```

Any [command](./cli.md) runs the same way, after `--`: `cargo run -p zmail-app -- tui`, `cargo run -p zmail-app -- list-accounts`. A release build is `cargo build -p zmail-app --release --locked`.

### Platform notes for building

On macOS, the first GPUI build may require Apple's Metal Toolchain. If `cargo check -p zmail-app` reports that the `metal` tool is missing, install it with:

```bash
xcodebuild -downloadComponent MetalToolchain
```

GitHub Actions now builds `zmail-app` on `ubuntu-latest`, `macos-latest`, and `windows-latest`, and publishes tagged release archives from the same matrix.

On Linux, the desktop dependency stack also needs D-Bus, Fontconfig, and FreeType development headers. The same packages used in CI are:

```bash
sudo apt-get update && sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype6-dev
```

HTML mail on Linux needs nothing extra to build; at runtime it needs WebKitGTK 4.1 and a working sandbox (see [Platform notes](./install.md#platform-notes) in the install guide).

The Rust toolchain in [`rust-toolchain.toml`](../rust-toolchain.toml) follows the one Zed pins for the GPUI release tag, so rustup selects it automatically.

## Live mail harness tests

Run the live Docker-backed mail harness tests:

```bash
scripts/mail-harness.sh up
cargo test -p zmail-test-support --test live_mail_harness -- --ignored --nocapture
cargo test -p zmail-test-support --test live_jmap_harness -- --ignored --nocapture
scripts/mail-harness.sh down
```

Phase 2b adds a repo-owned Docker Compose harness at [`docker/mail-harness/docker-compose.yml`](../docker/mail-harness/docker-compose.yml). It uses GreenMail for IMAP/SMTP and Apache James behind a local proxy for JMAP, with deterministic seeded state and automated account provisioning.

- IMAP endpoint: `imap://127.0.0.1:3143`
- SMTP endpoint: `127.0.0.1:3025`
- JMAP endpoint: `http://127.0.0.1:4080`
- mailbox login: `phase2`
- mailbox password: `phase2-password`
- mailbox address: `phase2@localhost`
- sender login: `sender`
- sender password: `sender-password`
- sender address: `sender@localhost`
- JMAP login: `jmap@localhost`
- JMAP password: `jmap-password`

The ignored integration tests start and stop the harness automatically. `live_mail_harness` verifies IMAP/SMTP round-tripping against GreenMail, `live_jmap_harness` verifies JMAP sync plus SMTP-delivered mail visibility against James, `live_tui_send` sends from the terminal UI's Compose, and `live_tui_setup` adds an account in the terminal UI's Setup → Accounts (connection test and first sync included).

[`docker/mail-harness/README.md`](../docker/mail-harness/README.md) has more on the harness (the Dovecot CONDSTORE and QRESYNC service, the IMAP watcher tests), and [`docker/proton-harness/README.md`](../docker/proton-harness/README.md) covers the Proton API harness.

## UI screenshots

Pull requests that change the UI get a `ui-screenshots` CI artefact: every main UI state in light and dark, plus narrow and wide windows and larger text sizes, captured on Linux from seeded fixture data with `zmail-app launch-ui-fixture VIEW [dark|light]` and [`scripts/capture-release-screenshot.sh`](../scripts/capture-release-screenshot.sh) `--matrix`. [`AGENTS.md`](../AGENTS.md) says how to run it locally and how to add a state. The desktop screenshots in the [README](../README.md) and the [install guide](./install.md#first-run) are states of that matrix too ([`docs/images`](./images)): each release captures and publishes them, and the documentation site uses the release's copies ([The documentation site](./site.md#screenshots) explains how, and how to refresh the committed ones). The terminal UI's screenshots there (`tui-*.png`) are rendered from its snapshot tests' screens by [`scripts/capture-tui-screenshots.sh`](../scripts/capture-tui-screenshots.sh), with no terminal or display ([Terminal UI screenshots](./site.md#terminal-ui-screenshots)).
