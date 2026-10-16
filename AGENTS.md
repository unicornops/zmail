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
- Release packaging ships platform archives plus native installers (Linux `.deb`, macOS DMG, Windows MSI); macOS releases are Developer ID signed and notarized when the org-wide Apple credentials (`APPLE_CERTIFICATE_BASE64` and friends, shared with the other UnicornOps macOS apps) are present, otherwise ad-hoc signed. Do not describe Windows code signing or `.deb` signing as complete.

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

Harness image pulls: the script pre-pulls the three Docker Hub images with
retries, and logs in with `DOCKERHUB_USERNAME`/`DOCKERHUB_TOKEN` when those
environment variables (repository secrets in CI) are configured, so pulls use
a personal quota instead of the shared anonymous one that exhausts on busy
GitHub runner pools (issue #88). Without credentials the pulls stay anonymous
and simply retry.

UI screenshot matrix (Linux, needs Xvfb, openbox, xdotool, ImageMagick and Mesa's lavapipe Vulkan driver, `mesa-vulkan-drivers`):

```bash
cargo build -p zmail-app --release --locked
xvfb-run -a --server-args="-screen 0 2560x1440x24" \
  scripts/capture-release-screenshot.sh --matrix artifacts/ui-screenshots
```

Each PNG is one UI state (empty, inbox, reader, compose, account setup) in dark and light, plus the reader at 900px and 1920px; `--list` prints the names and extra arguments capture only those. Each state is a `zmail-app launch-ui-fixture VIEW [dark|light]` launch: an in-memory store seeded with synthetic mail (`crates/zmail-app/src/ui_fixture.rs`), no sync, and the window opened on that view through `zmail_ui::InitialView`. CI runs it on pull requests that touch the UI, theme, assets or the capture script, and uploads the images as the `ui-screenshots` artefact of the CI run. Add new UI states to the fixture and the `MATRIX` list in the script.

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

GPUI pin:

- GPUI is a git dependency pinned to a Zed stable release tag (`tag = "vX.Y.Z"` on the `gpui` and `gpui_platform` lines of `Cargo.toml`), not the crates.io `gpui` crate. Keep both lines on the same tag.
- The weekly `zed-upstream` workflow (`scripts/bump-zed-gpui.sh`) opens a `chore(deps): bump Zed GPUI to vX.Y.Z` pull request when Zed ships a newer stable release; see `docs/upstream/zed.md` for manual bumps.
- Start the app with `gpui_platform::application()`. Do not use `Application::new_inaccessible`, which disables AccessKit screen-reader support.

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
