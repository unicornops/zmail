# Zmail

> [!WARNING]
> **Zmail is alpha software and is in no way ready for real use.** Expect bugs, missing features, breaking changes between releases and, at worst, lost or corrupted local data. Do not rely on it as your only mail client, keep your mail on the server, and keep backups of anything that matters. Native Proton Mail support is experimental on top of that (see below).

Zmail is a Rust desktop email client focused on speed, standards compliance, security, and large-mailbox performance. The UI is built on GPUI, unchanged Zed theme files are supported through a thin local compatibility layer, and the local data path is designed so network activity never blocks mailbox browsing.

## Current Status

Phases 1 through 4 are implemented in-repo, and most of Phase 5 hardening is in place:

- three-pane GPUI shell with local-first mailbox, thread, and preview browsing
- hydrated thread reader with safe HTML fallback rendering and remote-content blocking
- on macOS and Windows, HTML mail renders formatted in a locked-down web view inside the reader (`WKWebView`, or the system's WebView2 runtime on Windows; [ADR 0006](./docs/adr/0006-html-email-renderer.md)): JavaScript off, every load blocked except Zmail's own, links confirmed in Zmail's dialog, Zmail's menus and dialogs drawn above it through GPUI's native window composition (Zed PR #62379); remote images load only when you allow them, and then Zmail fetches them itself (https only, never from private addresses) so the web view never connects anywhere; each message's More menu switches to the Simple (text) view, and `ZMAIL_WEB_RENDERER=0` turns it off. On Linux the same locked-down page renders in WebKitGTK 4.1 (the `libwebkit2gtk-4.1-0` package, which the `.deb` recommends; 2.52.0 or newer): live inside the reader on X11, and as a picture of the page with working links and selection on Wayland. Zmail loads WebKitGTK only when it shows a formatted message and never runs it without its bubblewrap sandbox: without the library, below the minimum version, inside a snap or Flatpak, or where the sandbox cannot start (Ubuntu 24.04 restricts the user namespaces it needs), messages show as text with the reason. Windows without the WebView2 runtime keeps the text view
- SQLite-backed local mirror with migrations, paging, and persisted sync state
- local mailbox operations for read or unread, flag, move, archive, delete, and full-text search
- real IMAP and JMAP account sync behind a dedicated Tokio background runtime
- OAuth2 account setup in the settings dialog with provider presets (Gmail, Microsoft 365), background access-token refresh, and XOAUTH2 for IMAP sync, SMTP submission, and JMAP bearer authentication
- Gmail signs in with an app password by default; zmail has no Google OAuth client of its own yet, so [`docs/gmail-oauth.md`](./docs/gmail-oauth.md) (and the guide in the Add Account flow) walks through creating your own
- **experimental** native Proton Mail accounts (paid plans) that follow Proton Mail Bridge's behaviour without needing Bridge: sign-in with 2FA and human verification, sync (the newest mail first, older mail fetched in the background), live updates, actions and end-to-end encrypted sending ([ADR 0004](./docs/adr/0004-proton-native.md)). zmail is an independent client on Proton's private API and signs in with Proton Mail Bridge's app version, because Proton's sign-in accepts no other client, so Proton may change or block it; build with `--no-default-features` to leave it out
- received attachment download and save-as for IMAP and JMAP accounts
- full-text search with query operators; IMAP message bodies are fetched and indexed in the background, newest first, within the account's disk budget, pausing on battery power (Linux) unless allowed
- compose, autosave, attachment handling, outbox retry, SMTP submission, and JMAP submission support
- multiple identities from day one, with JMAP identity discovery and manual identity setup for generic accounts
- OS keychain-backed credential storage
- per-account cache policy persisted with each configured account
- unchanged Zed theme file parsing and semantic theme mapping
- coalesced UI refresh polling so background sync and outbox jobs do not flood the GUI thread
- multi-platform GitHub Actions builds plus Release Please automation starting at `0.0.1`
- Tagged releases also attach a Linux screenshot captured from the live mail harness by the Release Please workflow chain
- pull requests that change the UI get a `ui-screenshots` CI artefact: every main UI state in light and dark plus narrow and wide windows, captured from seeded fixture data (see `AGENTS.md`)
- release archives ship platform binaries plus a Linux `.deb`, a macOS DMG (Developer ID signed and notarized when the org-wide Apple credentials are present), and a Windows MSI installer; Windows code signing remains opt-in pending work
- screen-reader roles, names and announcements through AccessKit (VoiceOver, UI Automation, AT-SPI), Windows contrast themes and macOS Increase Contrast, reduced motion, text size up to 200% and 24px pointer targets; not yet checked with a real screen reader (see [`docs/phase5-hardening.md`](./docs/phase5-hardening.md#accessibility))
- Phase 5 hardening and release policy documentation

## Workspace

- [`PLAN.md`](./PLAN.md): master delivery plan
- [`docs/upstream/zed.md`](./docs/upstream/zed.md): Zed and GPUI upstream pin record (GPUI is normally pinned to a Zed stable release tag and bumped by a weekly workflow; for now it is pinned to Zed PR #62379 for the web renderer)
- [`docs/adr/0001-jmap-client-strategy.md`](./docs/adr/0001-jmap-client-strategy.md): JMAP implementation decision
- [`docs/adr/0003-gpui-zed-stable-pin.md`](./docs/adr/0003-gpui-zed-stable-pin.md): GPUI pinned to Zed stable release tags
- [`docs/phase5-hardening.md`](./docs/phase5-hardening.md): accessibility, performance, crash policy, packaging, and release process
- [`docs/performance.md`](./docs/performance.md): performance budgets, how they are measured, and the latest numbers
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

Search queries (in the app's search field and the `search` command) combine
words, `"quoted phrases"` and operators, all of which must match:
`from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `is:read`,
`is:flagged`, `in:<mailbox name or role>`, and `before:`/`after:` with a
`YYYY-MM-DD` date or a relative one such as `7d`, `2w`, `3m` or `1y`. A
leading `-` negates any of them:

```bash
cargo run -p zmail-app -- search 1 'from:alice "budget review" -in:junk after:30d'
```

In the app, a search starts in the folder the list shows (the Unified
Inbox counts as one folder: every account's inbox). The scope control under
the list header, the empty state and the Edit menu widen it to the folder's
account or to every account (Secondary+Alt+1, 2 and 3 pick folder, account
and all accounts). Each new search starts in the folder again. An `in:`
operator in the query names the folder itself and takes the folder's place
in the scope control. The `search` command searches one account.

The desktop app indexes bodies in the background; `zmail-app index ID` runs
one indexing pass for an account in the foreground and prints its progress.
Until every body is indexed, a search with free text also asks the server
(IMAP `UID SEARCH`, JMAP `Email/query`); its extra hits are listed after the
local ones and marked "found on server". The server search follows the
scope: in folder scope IMAP searches only that folder's mailboxes, and a
search across accounts asks each account's server separately, so one
offline account does not stop the others' results. `zmail-app search --server ID QUERY`
does the same from the command line.

Launch the desktop client:

```bash
cargo run -p zmail-app
```

The local store lives in a platform data directory so user data is independent of the launch directory:

- Linux: `$XDG_DATA_HOME/zmail` (default `~/.local/share/zmail`)
- macOS: `~/Library/Application Support/zmail`
- Windows: `%APPDATA%\zmail`

Set the `ZMAIL_DATA_DIR` environment variable to override the store directory for development. A legacy `.zmail-dev/zmail.sqlite3` store in the current directory is migrated to the platform data directory on first run; if the migration fails, the app keeps using the legacy store so no data is lost.

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
sudo apt-get update && sudo apt-get install -y pkg-config libdbus-1-dev libasound2-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev libfontconfig1-dev libfreetype6-dev
```

HTML mail on Linux (ADR 0006) uses WebKitGTK 4.1 at runtime; nothing is needed to build. To see formatted mail, install `libwebkit2gtk-4.1-0` (Debian 13, Ubuntu 24.04 and later; `webkit2gtk4.1` on Fedora). Its sandbox, bubblewrap, needs unprivileged user namespaces: it works on Debian, Fedora and Ubuntu 26.04, but not in most containers, and Ubuntu 24.04 allows them only to programs with an AppArmor profile that grants them. Where the sandbox cannot start, Zmail shows mail as text and says why; it never runs the engine without it. `ZMAIL_WEB_DEBUG=1` logs each step of the web renderer.

Window chrome: Zmail draws its own title bar with the toolbar inside it. On macOS the system title bar is transparent around the traffic lights; on Windows the caption buttons are drawn (Snap Layouts still work); on Linux Zmail draws a header bar, shadow and resize edges (client-side decorations) on GNOME and other desktops, and keeps the window manager's title bar on KDE and on X11 without a compositor or `_GTK_FRAME_EXTENTS` support. `ZMAIL_WINDOW_DECORATIONS=server` or `=client` overrides the choice (GNOME on Wayland has no server-side decorations, so it always gets the drawn ones).

GPUI draws through wgpu, which needs a Vulkan driver (the `libvulkan1` loader plus Mesa or a vendor driver) or, as a fallback, EGL/OpenGL at runtime. Desktop distributions ship these already; headless runs such as the release screenshot use Mesa's lavapipe software driver (`mesa-vulkan-drivers`).

The Rust toolchain in [`rust-toolchain.toml`](./rust-toolchain.toml) follows the one Zed pins for the GPUI release tag, so rustup selects it automatically.

## Licensing

Zmail is licensed under GPL-3.0-only. Dependency policy is enforced through [`deny.toml`](./deny.toml) and prepared for `cargo-about` via [`about.toml`](./about.toml).

Linux builds embed the [Inter](https://rsms.me/inter/) UI font (4.1, SIL Open Font License 1.1, [`assets/fonts/inter/LICENSE.txt`](./assets/fonts/inter/LICENSE.txt)); the `.deb` installs its licence under `/usr/share/doc/zmail-app/`. All builds embed [Lucide](https://lucide.dev) icons (ISC, with the Feather-derived icons also under MIT, [`assets/icons/LICENSE.txt`](./assets/icons/LICENSE.txt)), and code ported from GPUI is Apache-2.0 ([`crates/zmail-ui/LICENSE-APACHE`](./crates/zmail-ui/LICENSE-APACHE)). The local store is encrypted with [SQLCipher](https://www.zetetic.net/sqlcipher/) (BSD-3-Clause, [`crates/zmail-store/LICENSE-SQLCIPHER`](./crates/zmail-store/LICENSE-SQLCIPHER)), which on Linux and Windows links a statically built OpenSSL (Apache-2.0, [`crates/zmail-store/LICENSE-OPENSSL`](./crates/zmail-store/LICENSE-OPENSSL)); see [ADR 0005](./docs/adr/0005-store-encryption.md). Bundled non-crate assets and ported source files are listed in [`about.toml`](./about.toml).
