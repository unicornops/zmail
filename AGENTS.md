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
- `docs/adr/0004-proton-native.md` and `docs/upstream/proton.md`: native Proton Mail decision, upstream pins and ported files
- `docs/adr/0005-store-encryption.md`: encryption of the local store at rest (SQLCipher, encrypted attachment files, the store key in the OS keychain)

## Workspace Overview

- `crates/zmail-app`: app entry point and CLI
- `crates/zmail-ui`: GPUI shell and views
- `crates/zmail-store`: local SQLite-backed store
- `crates/zmail-sync`: background runtime and account sync orchestration
- `crates/zmail-imap`: IMAP adapter
- `crates/zmail-jmap`: JMAP adapter and submission support
- `crates/zmail-proton-api`: Proton Mail API client (go-proton-api port; not reachable from the UI until #139)
- `crates/zmail-proton`: Proton Mail sync adapter (Bridge services port), built only with `zmail-sync`'s `proton` feature, which `zmail-app` does not enable until #139
- `crates/zmail-smtp`: SMTP submission
- `crates/zmail-security`: keychain and rendering safety helpers
- `crates/zmail-theme`: Zed theme compatibility layer
- `crates/zmail-test-support`: synthetic data, the live mail harness and the Proton API harness

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

UI view tests use GPUI's test platform (`#[gpui::test]` with `TestAppContext` /
`VisualTestContext`, from the `test-support` feature in `zmail-ui`'s
dev-dependencies). They run headless with no display server; on Linux they only
need the build packages below. `crates/zmail-ui/src/view_tests.rs` opens the real
`WorkspaceView` over an ephemeral store, and
`crates/zmail-ui/src/components/text_input/tests.rs` drives a single component.

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

Proton API harness (go-proton-api's fake server, `docker/proton-harness`):

```bash
scripts/proton-harness.sh up      # build, start, wait, seed; state in target/proton-harness
cargo test -p zmail-test-support --test live_proton_harness -- --ignored --nocapture
cargo test -p zmail-test-support --test live_proton_api -- --ignored --nocapture
cargo test -p zmail-test-support --test live_proton_sync -- --ignored --nocapture
scripts/proton-harness.sh down
```

The live tests start the harness themselves through
`zmail_test_support::ProtonHarness`, which runs the script; `up` is only needed
to poke the API by hand (`scripts/proton-harness.sh env` prints
`PROTON_HARNESS_URL`, `PROTON_HARNESS_CA_PEM` and the seeded account). It
serves the API on `https://127.0.0.1:18443` with a certificate from a throwaway
CA made on every `up`; clients trust that CA only through `zmail-proton-api`'s
test-only `test-harness` feature, never in release builds. The image is built
locally from the pinned go-proton-api commit (CI builds it in the quality job);
where Docker is unavailable, `PROTON_HARNESS_MODE=native` builds and runs the
same pin with a local Go 1.26 toolchain. See `docker/proton-harness/README.md`.

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

Each PNG is one UI state (the first-run welcome, inbox, reader, compose, the Add Account window, the onboarding steps) in dark and light, plus the inbox and reader at the layout breakpoints (720, 900 and 1920px), the sidebar overlay at 900px, and three accounts with nested folders on the Unified Inbox, and each Settings window section; `--list` prints the names and extra arguments capture only those. Each state is a `zmail-app launch-ui-fixture VIEW [dark|light]` launch: an in-memory store seeded with synthetic mail (`crates/zmail-app/src/ui_fixture.rs`), no sync, and the window opened on that view through `zmail_ui::InitialView`. CI runs it on pull requests that touch the UI, theme, assets or the capture script, and uploads the images as the `ui-screenshots` artefact of the CI run. Add new UI states to the fixture and the `MATRIX` list in the script.

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

Proton upstream:

- Native Proton support ports Proton Mail Bridge and go-proton-api at the commits pinned between the `proton-pin` markers in `docs/upstream/proton.md`; `docs/upstream/proton-ports.toml` lists each ported upstream file and its zmail counterpart. Add an entry whenever you port a file.
- The weekly `proton-upstream` workflow (`scripts/check-proton-upstream.sh`) opens or updates one issue listing the ported files that changed upstream since the pins, with diff links, and any change to Bridge's SPKI pins. It never opens pull requests. `scripts/test-check-proton-upstream.sh` checks the script offline against `scripts/fixtures/proton-upstream`.

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
