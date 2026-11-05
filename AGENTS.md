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
- `docs/adr/0006-html-email-renderer.md`: rendering HTML mail (OS web engines behind a renderer boundary, the shared sanitiser and remote-content policy); tracked in #294
- `docs/adr/0007-terminal-ui.md`: a terminal UI as a second frontend (`zmail-tui`, `zmail-app tui`), terminal escape-sequence filtering, one syncing process; Pine/Alpine-style screens, Zellij-style copy and paste

## Workspace Overview

- `crates/zmail-app`: app entry point and CLI
- `crates/zmail-actions`: the undoable mail actions both frontends use (ADR 0007, section 8): archive, move, delete, flag and read with an undo stack, plus draft and outbox changes, each a store write with its queued write-back, reporting `MailEvent`s to a plain callback; and `reply`, what a reply, reply-all or forward draft starts with (recipients, following `Reply-To`, and a reply to the user's own message going to its recipients; the identity the message was addressed to; `Re:`/`Fwd:` subject; quoted or forwarded body; `In-Reply-To`/`References`; a forward's attachments), which `MailActions::create_started_draft` turns into a draft. No GPUI: `zmail-ui`'s `mail_actions.rs` wraps it in an entity whose callback is `cx.emit`, and `zmail-tui` keeps one for the session (triage and undo across screens)
- `crates/zmail-ui`: GPUI shell and views
- `crates/zmail-setup`: adding accounts, shared by both frontends (ADR 0007, section 8): the form's fields, validation and building `NewAccount` with its secret (`form`), provider presets and app password help (`providers`), autodiscovery driven step by step (`discovery`), the connection test before saving (`check`, through `check_sign_in` in `zmail-imap`, `zmail-smtp` and `zmail-jmap`), the OAuth2 browser sign-in step by step with the SSH note (`oauth`), the bring-your-own Google client guide as data (`google`), storing, removing and signing in again (`accounts`), how a JMAP identity that differs from its server is worded and the choices offered (`identities`), the identity editor's fields, validation, each account kind's rules (a linked JMAP identity's or a Proton address's address is fixed; Proton adds none; JMAP can create, update and delete on the server) and wording (`identity_editor`, #366), and the Proton sign-in state machine (`proton`, behind its `proton` feature). No GPUI; `zmail-ui`'s onboarding and Settings → Accounts draw it. Zmail ships no OAuth client ID or secret
- `crates/zmail-store`: local SQLite-backed store, with identities kept in step with a JMAP server's (`identities.rs`: each identity's server id and the server's values as last seen, so a sync adds new server identities, takes server-only changes, and keeps local edits marked as differing, `zmail_core::identity_sync`; the editor's writes: add, readdress, make primary, the removal rules; a server identity removed from Zmail only is hidden from later syncs until brought back (`hidden_identities`, matched by server id, else address); a Proton resync keeps the user's edits; a draft keeps the identity values it started with), and its advisory locks (`lock.rs`): `<database>.lock` (shared by every open store, exclusive for encrypting and key rotation) and `<database>.sync.lock` (`SyncLock`, ADR 0007: only its holder syncs)
- `crates/zmail-sync`: background runtime and account sync orchestration. Frontends build `SyncService::with_sync_lock`, which syncs only while it holds the store's sync lock, otherwise leaves sync to the holder (`runs_sync()` is false) and takes over when the holder quits. The CLI's commands that sync or send build `SyncService::for_command`, which holds the lock while the command runs or, when another process holds it, never takes it over: `sync`, `index` and `process-outbox` then stop with an error, and `send`, `retry-outbox`, `compose`, `add-imap`/`add-jmap` and `empty-trash` do their store or server part and leave syncing and sending to the holder, unless `--force` (then `SyncService::new`, which always syncs); commands that only read the store never touch the lock. Every sender renews its claim on an outbox job while the send runs (`LocalStore::renew_outbox_claim`), and the sync lock holder settles jobs left in `Sending` whose claim went silent, as a crash or a killed command leaves them (`src/interrupted.rs`, #403): from what the server shows for Proton (the draft's ExternalID) and JMAP (the Sent copy's `$submitpending`/`$submitted` keywords), and at most once for SMTP (failed with `zmail_core::INTERRUPTED_SEND_MESSAGE`, which asks the user to check Sent before retrying). A send that failed without proving the server refused it (a timeout, a dropped connection, a 5xx or 429 on Proton's `SendDraft`) is marked `zmail_core::UnconfirmedSend` and its job `send_unconfirmed` for good, so every later attempt makes the same lookup before sending anything (#405). A send that failed without proving the server refused it (a timeout, a dropped connection, a 5xx or 429 on Proton's `SendDraft`) is marked `zmail_core::UnconfirmedSend` and its job `send_unconfirmed` for good, so every later attempt makes the same lookup before sending anything (#405)
- `crates/zmail-imap`: IMAP adapter
- `crates/zmail-jmap`: JMAP adapter and submission support (a send imports its Sent copy marked `$submitpending`, which a successful `EmailSubmission/set` swaps for `$submitted`; a refused submission, `SubmissionRefused`, destroys the copy, and one that cannot be destroyed is left to the next sync's write-back; a retry first settles an earlier attempt by its copies' keywords, `resubmit_outgoing_message`, #406), and identities (`identities.rs`: `Identity/get` for the sync and submission, `Identity/set` update, create and destroy when the user sends an identity up or removes it there, all as plain JSON), and `batches.rs`: each `Email/get` asks for at most the session's `maxObjectsInGet` ids (250 at most) and halves the batch on `requestTooLarge`, for limits a server does not advertise (Apache James returns at most 5 emails with their bodies per call, #389)
- `crates/zmail-proton-api`: Proton Mail API client (go-proton-api port; not reachable from the UI until #139)
- `crates/zmail-proton`: Proton Mail sync adapter (Bridge services port), built only with `zmail-sync`'s `proton` feature. `zmail-ui` and `zmail-app` have a `proton` feature too (the Proton Mail provider in onboarding); it is off by default, so no release build has Proton until #139. CI lints `zmail-app --features proton` and runs the onboarding view tests with it
- `crates/zmail-smtp`: SMTP submission
- `crates/zmail-security`: keychain and rendering safety helpers, including the HTML mail sanitiser (`html`: ammonia allowlist, CSS pass, caps, link side table, remote-content policing) and its hostile-corpus tests (`tests/hostile_corpus.rs`, fixtures in `tests/fixtures/hostile`), what a sender-safety warning says for both frontends (`sender_warning`: tier, title, sentence, evidence, the Fewer Warnings level; the GUI's reader banner and the TUI draw it), and the terminal filter every string from mail goes through before a terminal draws it (`terminal::sanitize`, ADR 0007: controls, ESC and C1 sequences, bidi and zero-width characters) with its own corpus (`tests/terminal_corpus.rs`)
- `crates/zmail-render`: the renderer boundary of ADR 0006: `PreparedMessage` (sanitised HTML or why there is none, text, link table, `cid:` parts, remote content and the user's decision, theme) and `MessageRenderer`
- `crates/zmail-render-web`: the HTML mail web renderer (ADR 0006): a hardened web view per message. macOS: `WKWebView`, embedded through GPUI's native window composition. Windows: a WebView2 composition controller (a DirectComposition visual, with mouse input forwarded from GPUI); without the WebView2 runtime, the text view. Linux: WebKitGTK 4.1, loaded at runtime (`libloading`, never linked; missing, older than 2.52.0 or unable to start its bubblewrap sandbox means the text view, with the reason), on an engine thread of its own; a live GTK window in a native surface per message on X11, pictures of the page with a link map on Wayland. The platform FFI modules `src/platform/macos.rs`, `src/platform/windows.rs` and `src/platform/linux.rs` are the only modules in the workspace allowed `unsafe` (every block justified); every other crate keeps `unsafe_code = "forbid"`. `tests/canary.rs` (the macOS and Windows web renderer CI jobs) and `tests/canary_linux.rs` (Linux CI job: Xvfb and a headless weston on Ubuntu 26.04, the refusal on 24.04) render a hostile corpus and fail on any network request
- `crates/zmail-tui`: the terminal UI of ADR 0007, `zmail-app tui` behind `zmail-app`'s `tui` feature (on by default): Pine/Alpine screens on ratatui and crossterm, Zellij-style copy and paste, triage through `zmail-actions` (`src/triage.rs`), Pico-style compose on `ratatui-textarea` (`src/compose.rs`: drafts, reply and forward through `zmail_actions::reply`, attachments with `src/paths.rs`, send with undo-send through `SyncService::queue_draft_and_send_after`; the opt-in external editor in `src/editor.rs`, off by default, its temporary file in a fresh 0700 folder overwritten and removed when the editor exits) and the Outbox and postponed messages (`src/outbox.rs`), the user keymap file (`src/keymap.rs`: `tui-keymap.json` in the data directory, the GUI keymap's JSON format with a `context` per screen, `null` to unbind, ignored whole and reported when invalid; the bar, Main Menu, Help and palette show the user's keys), the `:` command palette over the screen's command-bar entries (`src/palette.rs`, matched with `zmail_core::fuzzy`, the GUI palette's matcher), sender-safety warnings in Message Text with the GUI's wording (`zmail_security::sender_warning`) and `X` Looks safe (`src/warnings.rs`), saving attachments from Message Text (`src/attachments.rs`: the Attachment Index on `V`, save one with Tab completion, asked before replacing a file, or all into a folder, through `AttachmentBlob::save_to`; never opened or launched), and Setup → Accounts (`src/accounts.rs`, drawn in `src/view/accounts.rs`: the accounts list with Add, Change (with the account's identities: update the server or use the server's for one that differs from its JMAP server; and the identity editor of `src/identities.rs`, drawn in `src/view/identities.rs`: edit, add, remove after asking and make primary, a labelled form with `^X` to save and the signature in `ratatui-textarea`, through `zmail_setup::identity_editor`) and Delete, Add step by step through `zmail-setup`, with discovery progress, the connection test, OAuth through the browser with the SSH note, and Proton's sign-in behind `zmail-tui`'s `proton` feature, which `zmail-app`'s turns on). It never depends on GPUI, `zmail-ui` or `zmail-render-web`; every string from mail goes through `zmail_security::terminal::sanitize`. `ratatui` `TestBackend` snapshots of each screen at 80x24 and 160x48 live in `src/snapshots` (`ZMAIL_UPDATE_SNAPSHOTS=1 cargo test -p zmail-tui` rewrites them; review the diff), and `zmail-app launch-tui-fixture [VIEW]` runs it on the GUI fixture's synthetic mail
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
cargo test -p zmail-test-support --test live_tui_send -- --ignored --nocapture
cargo test -p zmail-test-support --test live_tui_setup -- --ignored --nocapture
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

Each PNG is one UI state (the first-run welcome, inbox, reader, compose, the Add Account window, the onboarding steps) in dark and light, plus the inbox and reader at the layout breakpoints (720, 900 and 1920px), the sidebar overlay at 900px, and three accounts with nested folders on the Unified Inbox, each Settings window section, and the window that unlocks the encrypted store (encrypting an older store, a locked keychain, a lost key); `--list` prints the names and extra arguments capture only those. Each state is a `zmail-app launch-ui-fixture VIEW [dark|light]` launch: an in-memory store seeded with synthetic mail (`crates/zmail-app/src/ui_fixture.rs`), no sync, and the window opened on that view through `zmail_ui::InitialView`. CI runs it on pull requests that touch the UI, theme, assets, the capture scripts or the CI workflow, and uploads the images as the `ui-screenshots` artefact of the CI run. Add new UI states to the fixture and the `MATRIX` list in the script.

On Linux, CI's "Web renderer (Linux)" job runs `scripts/capture-linux-web-screenshots.sh` under Xvfb and openbox on Ubuntu 26.04: the same fixture states with HTML mail in a live WebKitGTK view, plus the wheel over a page, a link click and a selection copied with Ctrl-C (driven by xdotool; the clipboard is saved as `reader-html-copy.txt`), uploaded as the `linux-web-screenshots` artefact. With `--wayland` it runs the same states on Wayland, under weston's kiosk shell in a window on Xvfb (so xdotool still drives it), where pages are pictures; those PNGs are in the artefact's `wayland` folder with a `-wayland` suffix.

On macOS, CI's "Web renderer (macOS)" job also runs `scripts/capture-macos-web-screenshots.sh`: the `reader-html`, `reader-html-menu`, `reader-html-scrolled` and `reader-link-warning` fixture states, with HTML mail in the web renderer, captured with `screencapture`, plus three interactions driven by posted events (`scripts/macos-ui-events.swift`: the wheel over a page, a link click, a selection copied with Command-C, with the clipboard saved as `reader-html-copy.txt`), uploaded as the `macos-web-screenshots` artefact (each PNG with the launch's `ZMAIL_WEB_DEBUG` log).

On Windows, CI's "Web renderer (Windows)" job runs the canary (`scripts/run-windows-canary.ps1`) on the runner image's WebView2 runtime, updates the runtime to the latest with Microsoft's Evergreen bootstrapper (`scripts/update-webview2-runtime.ps1`, which logs the version before and after) and runs the canary again, so CI always tests the runtime users get with Edge. It then runs `scripts/capture-windows-web-screenshots.ps1`: the same fixture states with HTML mail in WebView2, captured from the screen, plus the same three interactions and a switch to another conversation, driven by synthesised input relative to the window (the clipboard saved as `reader-html-copy.txt`), uploaded as the `windows-web-screenshots` artefact (each PNG with its `ZMAIL_WEB_DEBUG` log and the window's screen rectangle).

Dependency policy:

```bash
cargo deny check licenses advisories bans sources
cargo audit
```

Dependabot (`.github/dependabot.yml`) opens weekly update pull requests for Cargo (`fix(deps)`, minor and patch grouped) and GitHub Actions (`ci(deps)`), plus security updates for Dependabot alerts. It skips the GPUI pin and the `pgp` and `proton-srp` forks; move those by hand. The fuzz crate's own lockfile gets one grouped `test(deps)` pull request a month.

Fuzzing (nightly, `cargo install cargo-fuzz`; see `fuzz/README.md` for the SIMD tiers and findings):

```bash
cd fuzz && cargo +nightly fuzz run -O -a zmail_parse corpus/zmail_parse -- -runs=0
```

`fuzz/` is a cargo-fuzz crate outside the workspace with its own `Cargo.lock`: `zmail_parse` drives `zmail_imap::hydrated_body_from_rfc822` and mail-parser on hostile mail, `encodify_decoders` the decoders below it. The `Fuzz` workflow (`.github/workflows/fuzz.yml`) builds them, replays `fuzz/corpus` and fuzzes for five minutes when `fuzz/`, zmail-imap, zmail-core, zmail-security or the mail-parser, encodify or simdutf8 versions change, and weekly.

Linux CI packages:

```bash
sudo apt-get update && sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype6-dev
```

The Linux web renderer needs only the runtime package `libwebkit2gtk-4.1-0` (it brings `bubblewrap` and `xdg-dbus-proxy`), never `-dev` headers; CI's "Web renderer (Linux)" job adds it with `xvfb` and `weston`, and for its screenshots openbox, xdotool, xclip, ImageMagick and Mesa's Vulkan drivers. Its sandbox needs unprivileged user namespaces, so it cannot run in an unprivileged container (the engine then refuses, by design: there is no unsandboxed mode) and Ubuntu 24.04 refuses it without an AppArmor profile for Zmail: the `.deb` installs `crates/zmail-app/packaging/apparmor/zmail` as `/etc/apparmor.d/zmail` and loads it in `postinst` (unloads it in `postrm`) (`crates/zmail-app/packaging/debian`), and CI's Ubuntu 24.04 job checks that profile lets the canary's sandbox start.

macOS note:

- GPUI may need the Metal toolchain installed with `xcodebuild -downloadComponent MetalToolchain`.

GPUI pin:

- GPUI is a git dependency pinned to a Zed stable release tag (`tag = "vX.Y.Z"` on the `gpui` and `gpui_platform` lines of `Cargo.toml`), not the crates.io `gpui` crate. Keep both lines on the same tag.
- Current deviation: for the web renderer (ADR 0006) both lines pin the head of Zed PR #62379 (native window composition) by `rev` on the `huacnlee/zed` fork, until a Zed stable release contains it; see "Deviation From ADR 0003" in `docs/upstream/zed.md`. Keep both lines on the same commit.
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
