# Zmail

Zmail is a Rust desktop email client focused on speed, standards compliance, security, and large-mailbox performance. The UI is built on GPUI, unchanged Zed theme files are supported through a thin local compatibility layer, and the local data path is designed so network activity never blocks mailbox browsing.

## Current Status

Phases 1 through 4 are implemented in-repo, and most of Phase 5 hardening is in place:

- three-pane GPUI shell with local-first mailbox, thread, and preview browsing
- hydrated thread reader with safe HTML fallback rendering and remote-content blocking
- SQLite-backed local mirror with migrations, paging, and persisted sync state
- local mailbox operations for read or unread, flag, move, archive, delete, and full-text search
- real IMAP and JMAP account sync behind a dedicated Tokio background runtime
- compose, autosave, attachment handling, outbox retry, SMTP submission, and JMAP submission support
- multiple identities from day one, with JMAP identity discovery and manual identity setup for generic accounts
- OS keychain-backed credential storage
- per-account cache policy persisted with each configured account
- unchanged Zed theme file parsing and semantic theme mapping
- coalesced UI refresh polling so background sync and outbox jobs do not flood the GUI thread
- multi-platform GitHub Actions builds plus Release Please automation starting at `0.0.1`
- release archives currently ship raw platform binaries; native installers and signing remain pending release work
- Phase 5 hardening and release policy documentation

## Workspace

- [`PLAN.md`](./PLAN.md): master delivery plan
- [`docs/upstream/zed.md`](./docs/upstream/zed.md): Zed and GPUI upstream pin record
- [`docs/adr/0001-jmap-client-strategy.md`](./docs/adr/0001-jmap-client-strategy.md): JMAP implementation decision
- [`docs/phase5-hardening.md`](./docs/phase5-hardening.md): accessibility, performance, crash policy, packaging, and release process
- [`themes/phase0-dark.json`](./themes/phase0-dark.json): sample unchanged Zed-format theme file

## Commands

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

Run the live Docker-backed mail harness tests:

```bash
scripts/mail-harness.sh up
cargo test -p zmail-test-support --test live_mail_harness -- --ignored --nocapture
cargo test -p zmail-test-support --test live_jmap_harness -- --ignored --nocapture
scripts/mail-harness.sh down
```

Add and sync an account:

```bash
cargo run -p zmail-app -- add-imap
cargo run -p zmail-app -- add-jmap
```

Inspect configured accounts or trigger sync:

```bash
cargo run -p zmail-app -- list-accounts
cargo run -p zmail-app -- sync all
cargo run -p zmail-app -- sync 1
```

Search or operate on locally mirrored mail:

```bash
cargo run -p zmail-app -- search 1 incident
cargo run -p zmail-app -- read 42
cargo run -p zmail-app -- flag 42
cargo run -p zmail-app -- mark-read 42
cargo run -p zmail-app -- move 42 7
cargo run -p zmail-app -- archive 42
cargo run -p zmail-app -- delete 42
```

Launch the desktop client:

```bash
cargo run -p zmail-app
```

By default, the development app persists its local store at `.zmail-dev/zmail.sqlite3`.

The desktop shell no longer creates demo accounts from inside the UI. Configure accounts through the CLI commands above, then use the app to browse and refresh the local mirror.

## Mail Harness

Phase 2b adds a repo-owned Docker Compose harness at [`docker/mail-harness/docker-compose.yml`](./docker/mail-harness/docker-compose.yml). It uses GreenMail for IMAP/SMTP and Apache James behind a local proxy for JMAP, with deterministic seeded state and automated account provisioning.

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

The ignored integration tests start and stop the harness automatically. `live_mail_harness` verifies IMAP/SMTP round-tripping against GreenMail, and `live_jmap_harness` verifies JMAP sync plus SMTP-delivered mail visibility against James.

## Platform Notes

On macOS, the first GPUI build may require Apple's Metal Toolchain. If `cargo check -p zmail-app` reports that the `metal` tool is missing, install it with:

```bash
xcodebuild -downloadComponent MetalToolchain
```

GitHub Actions now builds `zmail-app` on `ubuntu-latest`, `macos-latest`, and `windows-latest`, and publishes tagged release archives from the same matrix.

On Linux, the desktop dependency stack also needs D-Bus, Fontconfig, and FreeType development headers. The same packages used in CI are:

```bash
sudo apt-get update && sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxkbcommon-dev libfontconfig1-dev libfreetype6-dev
```

## Licensing

Zmail is licensed under GPL-3.0-only. Dependency policy is enforced through [`deny.toml`](./deny.toml) and prepared for `cargo-about` via [`about.toml`](./about.toml).
