# What Zmail has

Zmail is a Rust desktop email client focused on speed, standards compliance, security, and large-mailbox performance. The UI is built on GPUI, unchanged Zed theme files are supported through a thin local compatibility layer, and the local data path is designed so network activity never blocks mailbox browsing.

What is built so far (Zmail is beta, so expect gaps and rough edges):

- three-pane GPUI shell with local-first mailbox, thread, and preview browsing
- hydrated thread reader with safe HTML fallback rendering and remote-content blocking
- on macOS and Windows, HTML mail renders formatted in a locked-down web view inside the reader (`WKWebView`, or the system's WebView2 runtime on Windows; [ADR 0006](./adr/0006-html-email-renderer.md)): JavaScript off, every load blocked except Zmail's own, links confirmed in Zmail's dialog, Zmail's menus and dialogs drawn above it through GPUI's native window composition (Zed PR #62379); remote images load only when you allow them, and then Zmail fetches them itself (https only, never from private addresses, raster images only, at most 100 per message) so the web view never connects anywhere; each message's More menu switches to the Simple (text) view, Settings → Mail shows all HTML mail as plain text (see [below](#settings--mail)), and `ZMAIL_WEB_RENDERER=0` turns the web view off whatever Settings says. On Linux the same locked-down page renders in WebKitGTK 4.1 (the `libwebkit2gtk-4.1-0` package, which the `.deb` recommends; 2.52.0 or newer): live inside the reader on X11, and as a picture of the page with working links and selection on Wayland. Zmail loads WebKitGTK only when it shows a formatted message and never runs it without its bubblewrap sandbox: without the library, below the minimum version, inside a snap or Flatpak, or where the sandbox cannot start (Ubuntu 24.04 restricts the user namespaces it needs), messages show as text with the reason. Windows without the WebView2 runtime keeps the text view
- SQLite-backed local mirror, encrypted at rest with SQLCipher ([ADR 0005](./adr/0005-store-encryption.md)), with migrations, paging, and persisted sync state
- local mailbox operations for read or unread, flag, move, archive, delete, and full-text search
- real IMAP and JMAP account sync behind a dedicated Tokio background runtime
- account setup in the app itself (first-run onboarding, then the Add Account window), with provider presets (Gmail, Microsoft 365), background access-token refresh, and XOAUTH2 for IMAP sync, SMTP submission, and JMAP bearer authentication
- Gmail signs in with an app password by default; Zmail has no Google OAuth client of its own yet, so [`docs/gmail-oauth.md`](./gmail-oauth.md) (and the guide in the Add Account flow) walks through creating your own
- **experimental** native Proton Mail accounts (paid plans) that follow Proton Mail Bridge's behaviour without needing Bridge: sign-in with 2FA and human verification, sync (the newest mail first, older mail fetched in the background), live updates, actions and end-to-end encrypted sending ([ADR 0004](./adr/0004-proton-native.md)). Zmail is an independent client on Proton's private API and signs in with Proton Mail Bridge's app version, because Proton's sign-in accepts no other client, so Proton may change or block it; build with `--no-default-features` to leave it out
- received attachment download and save-as for IMAP and JMAP accounts
- full-text search with query operators ([Searching mail](./search.md)); IMAP message bodies are fetched and indexed in the background, newest first, within the account's disk budget, pausing on battery power (Linux) unless Settings → Mail says to keep going
- compose, autosave, attachment handling, outbox retry, SMTP submission, and JMAP submission support
- Zmail as your mail app: a `mailto:` link from a browser or another app, or one in a message (after the link dialog), opens a new message in Zmail, filled from the link; while Zmail is open, the running app takes the link ([Make Zmail your mail app](./install.md#make-zmail-your-mail-app))
- multiple identities from day one, with JMAP identity discovery and manual identity setup for generic accounts
- OS keychain-backed credential storage
- per-account cache policy persisted with each configured account
- unchanged Zed theme file parsing and semantic theme mapping
- coalesced UI refresh polling so background sync and outbox jobs do not flood the GUI thread
- multi-platform GitHub Actions builds plus Release Please automation starting at `0.0.1`
- Tagged releases also attach a Linux screenshot captured from the live mail harness by the Release Please workflow chain
- pull requests that change the UI get a `ui-screenshots` CI artefact: every main UI state in light and dark plus narrow and wide windows, captured from seeded fixture data (see [Development](./development.md#ui-screenshots))
- release archives ship platform binaries plus a Linux `.deb`, a macOS DMG (Developer ID signed and notarised), and a Windows MSI installer, each with checksums and a build provenance attestation; the Windows and Linux downloads are not signed (see [Installing Zmail](./install.md))
- a terminal UI for reading, triaging and writing mail (with saving attachments, sender-safety warnings, a `:` command palette and a user keymap file), `zmail-app tui` ([ADR 0007](./adr/0007-terminal-ui.md); see [Terminal UI](./tui.md)): Pine/Alpine-style screens on the same encrypted store, every string from mail filtered for escape sequences before it reaches the terminal; delete, archive, save to a folder, flag and mark read with undo, sharing the desktop app's mail actions; Pico-style compose, reply and forward with drafts, attachments, undo-send and an Outbox; and Setup → Accounts to add (with autodiscovery, a connection test, OAuth through the browser, and Proton sign-in in builds with Proton), change and delete accounts, through the same account setup code as the desktop app (`zmail-setup`)
- screen-reader roles, names and announcements through AccessKit (VoiceOver, UI Automation, AT-SPI), Windows contrast themes and macOS Increase Contrast, reduced motion, text size up to 200% and 24px pointer targets; not yet checked with a real screen reader (see [`docs/phase5-hardening.md`](./phase5-hardening.md#accessibility))
- Phase 5 hardening and release policy documentation

## Settings → Mail

The desktop app's Settings window has a Mail section. Each choice is saved and applied as soon as it changes:

- **HTML mail:** Formatted shows a message's HTML in the locked-down web view above, with remote content blocked until you allow it; As Plain Text shows only its text, without pictures or layout, for every message. Applies at once. When `ZMAIL_WEB_RENDERER=0` is set, or the build has no web renderer, HTML mail is always plain text: the row says why and its picker is disabled. A message's More menu still switches one message between the two views.
- **New mail notifications:** On (the default) or Off. A system notification names the sender and subject when new mail arrives while Zmail is not the window in front ([notification policy](./phase5-hardening.md#new-mail-notification-policy)). Applies from the next new mail.
- **Background indexing:** downloads the messages you have not opened, newest first and within each account's cache budget, so [search](./search.md) can find words in them (IMAP accounts only; other accounts download whole messages as they sync). Turning it off stops after the current batch; turning it on resumes after the next sync.
  - **On battery power:** Pause (the default) or Keep Indexing. Zmail can tell it runs on battery only on Linux so far; elsewhere indexing never pauses for this.
  - **On metered connections:** Pause (the default) or Keep Indexing. Zmail cannot tell yet when a connection is metered, so for now this changes nothing.
