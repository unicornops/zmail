# AGENTS.md

Shared instructions for human and AI contributors working in this repository.

## What This Repo Is

Zmail is a Rust desktop email client focused on:

- fast local-first mailbox browsing
- standards-compliant IMAP, SMTP, and JMAP support
- a GPUI-based cross-platform desktop shell for macOS, Linux, and Windows
- security-first rendering and message handling
- large-mailbox performance

The app is organized as a Rust workspace with separate crates for UI, store, sync, protocol adapters, theming, security, and test support.

## Important Files

- `README.md`: current project status, commands, and platform notes
- `PLAN.md`: product phases and scope
- `docs/phase5-hardening.md`: current hardening and packaging status
- `docs/upstream/zed.md`: pinned upstream GPUI and Zed context
- `docs/adr/0001-jmap-client-strategy.md`: JMAP implementation decision

## Workspace Overview

- `crates/zmail-app`: app entry point and CLI
- `crates/zmail-ui`: GPUI shell and views
- `crates/zmail-store`: local SQLite-backed store
- `crates/zmail-sync`: background runtime and account sync orchestration
- `crates/zmail-imap`: IMAP adapter
- `crates/zmail-jmap`: JMAP adapter and submission support
- `crates/zmail-smtp`: SMTP submission
- `crates/zmail-security`: keychain and rendering safety helpers
- `crates/zmail-theme`: Zed theme compatibility layer
- `crates/zmail-test-support`: synthetic data and live mail harness support

## Repo Expectations For Agents

- Keep the local-first design intact. The UI should read local state, not remote state.
- Preserve the modular boundary between UI, store, sync, and protocol crates.
- Treat IMAP and JMAP as first-class supported protocols.
- Keep security defaults strict. Do not weaken TLS, remote-content blocking, or message sanitization casually.
- Release packaging ships platform archives plus native installers (Linux `.deb`, macOS `.app`, Windows MSI); code signing and macOS notarization remain opt-in pending work. Do not describe signing or notarization as complete.

## Commands

Core verification:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Release build:

```bash
cargo build -p zmail-app --release --locked
```

Live protocol harness:

```bash
scripts/mail-harness.sh up
cargo test -p zmail-test-support --test live_mail_harness -- --ignored --nocapture
cargo test -p zmail-test-support --test live_jmap_harness -- --ignored --nocapture
scripts/mail-harness.sh down
```

Dependency policy:

```bash
cargo deny check licenses advisories bans sources
cargo audit
```

Linux CI packages:

```bash
sudo apt-get update && sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype6-dev
```

macOS note:

- GPUI may need the Metal toolchain installed with `xcodebuild -downloadComponent MetalToolchain`.

## Commit Policy

Conventional Commits must always be used.

Use formats like:

- `feat: add account sync retry handling`
- `fix: handle empty JMAP message body values`
- `docs: clarify packaging status`
- `ci: add Linux font dependencies`

Do not create non-conventional commit messages in this repository.

## Related Agent Files

- [`CLAUDE.md`](./CLAUDE.md)
- [`.github/copilot-instructions.md`](./.github/copilot-instructions.md)

Those files should stay aligned with this document and point back here instead of drifting into separate instruction sets.
