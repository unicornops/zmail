# Phase 5 Hardening

This document records the Phase 5 hardening work that is present in-repo today and calls out the remaining packaging gaps from [`PLAN.md`](../PLAN.md).

## Accessibility

This section records what ships today, so nobody relies on support that does not exist. None of the screen-reader support below has been checked with a real screen reader yet; [Manual screen-reader check](#manual-screen-reader-check) is the script for doing that.

### Keyboard

- Every command is a GPUI action with a key binding (`crates/zmail-ui/keymaps/default.json`, plus `macos.json` on macOS), a menu entry and a command palette entry (Ctrl+K, Cmd+K on macOS). Settings → Keyboard offers an optional Gmail keymap. Ctrl+/ (Cmd+/) and the Gmail keymap's `?` open a cheat sheet of the bound keys.
- Each pane (sidebar, message list, reader, compose) has its own focus. Tab and F6 move between panes, and Shift+Tab and Shift+F6 move back. Arrow keys, Home and End move the selection in the focused pane, and Enter opens it.
- Text fields are real editors, with selection, copy, cut, paste, undo and redo, word movement and IME input.
- Buttons, selects, menus and dialogs are Tab stops with a visible 2px focus ring. Enter and Space activate buttons. Escape closes the topmost overlay.
- **Pointer only:** the full date of a list row is a tooltip, so it needs hovering; screen readers get it as the row's description. The row's hover quick actions (Archive, Delete, Flag, Read) have keyboard shortcuts and context-menu equivalents instead.

### Screen readers

GPUI (pinned to a Zed stable release, see `docs/upstream/zed.md`) ships AccessKit, and Zmail starts it with `gpui_platform::application()`, which leaves it enabled. AccessKit drives VoiceOver on macOS, UI Automation (NVDA, Narrator, JAWS) on Windows and AT-SPI (Orca) on Linux.

- **Names and roles.** Every control reports a role and a name (`crates/zmail-ui/src/a11y.rs`). Icon-only buttons are named by their tooltip, and shortcuts are spelled out ("Shift+Command+K", "Ctrl+Shift+K"). Avatars, badges and key caps are decorative; the row or button around them says what they mean.
- **Structure.**
  - The sidebar is a tree: levels, expanded state, and Expand/Collapse actions. Rows read "Unified Inbox, 5 unread" or "Work account, Updated just now".
  - The message list is a multi-select list box. Rows read "Unread, from Alice, Lunch on Friday, 10:42, has attachments", with the preview line and the full date as the description.
  - The reader is a pane with a "Conversation actions" toolbar, the subject as a heading, and each message as an article ("Unread message, from Alice, …") that can be expanded and collapsed.
  - Compose is a form named after its subject.
  - Settings is a tab list of sections, with headings. Each account's controls are grouped under the account's name.
  - Dialogs are modal and named by their title.
- **Keyboard navigation is announced.** In the sidebar, the message list, the reader, menus and the Settings sections, focus stays on the pane while the arrow keys (and j/k, n/p) move. The current row is reported as the focused item.
- **Announcements (live regions).**
  - Toasts and status messages are announced politely, and errors (error toasts, the error banner, a failed send, account-setup field errors) assertively.
  - The result count of each new search is announced.
  - Sync is announced only when an account starts failing or recovers; routine background syncs stay quiet.
  - In the command palette, Move to… and recipient suggestions, the highlighted option is announced ("Archive, 1 of 12"). Focus stays in the text field there, and GPUI only reports a focused list item under the focused element.
- **Readable text.** Headings, empty states, the open-link prompt's full address, a message's From, To and Date details and View Source are text a screen reader can reach. Message bodies are read paragraph by paragraph.
- **Not yet:**
  - Links inside a message body are not separate links a screen reader can follow; use Open First Link or the link prompt.
  - Text fields report their text but not the caret or selection.
  - List rows say "has attachments", not how many, because the store keeps only a flag per conversation.
  - The tree and list cannot be tested automatically, because GPUI's test platform never builds the accessibility tree. View tests check instead that every control the main window, Settings and onboarding draw has a non-empty name (`crates/zmail-ui/src/view_tests/a11y.rs`).

### Visual

- **Theme.** The theme follows the system light/dark appearance until it is switched with Ctrl+Shift+T (Cmd+Shift+T on macOS) or View → Switch Light/Dark Theme; the choice is saved in the UI preferences. Body text, controls and focus rings are checked against WCAG contrast minimums (4.5:1 text, 3:1 boundaries) for the bundled palettes.
- **Windows contrast themes** (High Contrast, or Windows 11's Aquatic, Desert, Dusk and Night sky):
  - When one is on, every colour comes from the user's system colours, read from `HKCU\Control Panel\Colors`.
  - Status colours fall back to the text colour, as in Windows' own controls; banners still differ by their icons.
  - The setting is read at startup and again when the window is activated or the appearance changes.
- **macOS Increase Contrast** (System Settings → Accessibility → Display) keeps the theme but raises secondary text to 7:1 and borders to 4.5:1. It is read at the same times.
- **Linux** has no high-contrast handling yet.
- **Reduced motion.** Settings → Appearance → Motion offers Match System, Reduce and Full.
  - Match System follows:
    - macOS: Reduce motion (`NSWorkspace.accessibilityDisplayShouldReduceMotion`);
    - GNOME: `org.gnome.desktop.interface enable-animations`, read by a short-lived `gsettings`;
    - Windows: `HKCU\Control Panel\Desktop\WindowMetrics\MinAnimate`, as a proxy for Settings → Accessibility → Visual effects → Animation effects. Turning that switch off clears it. The value itself is the minimise and maximise animation, so turning off only the client-area animations in Performance Options is not detected, and turning off only the minimise animation reduces motion here.
  - The setting is read at startup and whenever the window is activated. A setting that cannot be read allows motion.
  - With motion reduced, the loading placeholders stop pulsing, spinners stand still and animated images show their first frame.
  - Nothing in the UI slides: menus, overlays, dialogs and toasts appear and go at once. `crates/zmail-ui/src/motion.rs` lists every animation.
- **Text size.** Settings → Appearance → Text size scales text in every window from 100% to 200%, in 25% steps. It sets GPUI's rem size.
  - Rows, buttons, selects, menus and the widths of dialogs, popovers and cards grow with the text. Spacing, borders and icons stay the same size. Settings rows wrap their control below the text when both do not fit.
  - The main views are captured at 150% and 200% in the screenshot matrix, and view tests check that list rows grow to hold the larger text.
  - GPUI exposes no OS text-scale setting, so the system's text size (macOS, Windows "Text size", GNOME's text scaling factor) is not followed yet; use the setting.

### Pointer targets

- Everything that takes a click is at least 24×24 px at the default text size (WCAG 2.2, 2.5.8 Target Size (Minimum)), and larger text only makes targets bigger. `crates/zmail-ui/src/view_tests/hit_targets.rs` checks every target the main window, onboarding, each Settings section, the compose window and the command palette draw.
- Two exceptions are documented in `crates/zmail-ui/src/components/hit_target.rs` rather than counted as passes. Each has keyboard and wheel alternatives:
  - The split handles between panes are 6 px wide. Arrow keys, Home and End resize them, and a double click restores the default width.
  - The overlay scrollbar's strip is 12–15 px wide, though its thumb is at least 24 px long. The wheel, the trackpad and Page Up/Down scroll.

### Content

- The reader renders message bodies as text, and HTML is converted to text before display.
- Remote content stays blocked by default, and attachments are only saved on explicit user action.

### Manual screen-reader check

Run this on each platform before claiming screen-reader support works, and note anything that is silent or wrong. Start with an account that has mail in more than one folder.

1. **Start the screen reader.**
   - macOS: VoiceOver, Cmd+F5.
   - Windows: NVDA (Ctrl+Alt+N) or Narrator (Win+Ctrl+Enter).
   - Linux: Orca, Super+Alt+S, on a GNOME session.

   Then open Zmail. The window should be announced as "Zmail".
2. **Sidebar.** Press F6 until the sidebar has focus. Up and Down should read each row with its unread count and level ("Unified Inbox, 5 unread"). On an account row, Enter (or the screen reader's expand command) should open and close it and announce "expanded" or "collapsed".
3. **Message list.** Press Tab to the list. Up and Down should read "Unread, from …, subject, time, …" and "selected". Shift+Down should extend the selection and still read each row.
4. **Reader.** Press Enter, then Tab to the reader. The subject should read as a heading, and n and p should move between messages ("Unread message, from …, date"). The screen reader's reading command should read the body.
5. **Actions.** Press Ctrl+E (Cmd+E) to archive. The "Conversation archived" toast should be announced without moving focus. Press Ctrl+Z (Cmd+Z) to undo.
6. **Search.** Press Ctrl+F (Cmd+F) and type a word that matches, then press Enter. The result count should be announced ("3 results").
7. **Palette.** Press Ctrl+K (Cmd+K) and type "arch". Each Down press should announce the highlighted command ("Archive, 2 of 5"). Escape closes it.
8. **Compose.** Press Ctrl+N (Cmd+N). The To, Subject and Message fields should each read their name. Add a recipient, then Tab to the chip's Remove button, which should read "Remove <name>".
9. **Errors.** In Add Account (Ctrl+Shift+A, Cmd+Shift+A), press Continue with the fields empty. The field error should be announced at once.
10. **Settings.** Press Ctrl+, (Cmd+,). Up and Down in the section list should read each tab, and Tab should move into the section, whose controls each read a name.
11. **High contrast (Windows).** Turn on a contrast theme (Settings → Accessibility → Contrast themes) and switch back to Zmail. The window should redraw in the theme's colours, and the selected row should stay readable.
12. **Increase Contrast (macOS).** Turn Increase contrast on and switch back to Zmail. Secondary text and borders should darken (in a light theme) or lighten (in a dark theme).
13. **Reduced motion.** Turn the system's reduce-motion setting on and switch back to Zmail. Press F5 to sync: the toolbar's sync indicator should stand still instead of spinning.
14. **Text size.** In Settings → Appearance, set Text size to 200%. Every window should grow its text, and nothing in the main views should be cut off at 1280×800.

## Performance

- Network sync and outbox delivery stay on the dedicated Tokio background runtime in `zmail-sync`.
- The shell only schedules one delayed refresh at a time while background work is active.
- Local browsing, mailbox switching, and search continue to read from the local mirror rather than from network paths.
- Phase 5 adds tests that assert sync and outbox scheduling return immediately instead of waiting on slow background work.
- UI plan Phase 8 sets numeric budgets over a 100,000-conversation mailbox: 8.3 ms per list scroll frame, and 100 ms from an action (archive, flag, mark read, opening a folder or conversation, a search keystroke, opening a heavy reader) to the visible change.
  - [`docs/performance.md`](./performance.md) records the budgets, how they are measured and the latest numbers.
  - CI runs the budgets on Linux with a gross-regression gate.
  - Dragging the scrollbar across a 100k mailbox still misses the frame budget (#242). Wheel and keyboard scrolling stay within it.
  - In search, a short word typed after a full one (`budget r`) can take just over the 100 ms budget in a 100k folder (#250). Other keystrokes take 25–45 ms.

## Mailbox Deletion Policy

- Deletion moves messages to the account's trash mailbox whenever the server reports one; the write-back queue applies the move to the server on the next sync.
- When no trash mailbox exists, destroyed messages are only marked `\Deleted` on the server — never expunged — so deletion stays reversible.
- Expunging happens exclusively through the explicit `zmail-app empty-trash ACCOUNT` command: it selects the trash mailbox read-write, marks every message `\Deleted`, and issues `EXPUNGE` within that mailbox only, followed by a sync to refresh the local mirror.
- JMAP accounts have no dangling state: `Email/set` destroy removes messages server-side immediately, and trash semantics follow the server's own handling.

## New Mail Notification Policy

- Notifications are raised by the `zmail-notify` crate through `notify-rust`, which resolves to D-Bus `org.freedesktop.Notifications` on Linux, the notification center on macOS, and toast notifications on Windows.
- One notification is raised per arrival batch — one sync that found new mail — naming the account and up to three arrival subjects.
- The first sync of a mailbox counts as an initial import, not new mail, and never notifies; reconnecting a large account therefore cannot flood the user.
- Notifications are capped at six per rolling minute; batches beyond the cap collapse into a summary line until the window frees up.
- The GUI suppresses notifications while its window is focused (the quiet flag mirrors window activation), so mail arriving while the user reads it does not interrupt.
- A missing notification daemon or a failed notification never fails the sync path; the failure is dropped.

## Crash And Error Reporting Policy

- Zmail does not upload crash reports or telemetry automatically.
- Errors stay local to the terminal, local logs, and GitHub issues opened by users.
- GUI installs a panic hook that appends one line per panic (timestamp, thread, location, message) to a crash log in the platform's state directory — `~/Library/Logs/zmail/crash.log` on macOS, `$XDG_STATE_HOME/zmail/crash.log` (default `~/.local/state/zmail/`) on Linux, and `%LOCALAPPDATA%\zmail\logs\crash.log` on Windows — because GUI launches from Finder, the Dock, or installers have no stderr to print to. The log rotates at 1 MiB (`crash.log.old` keeps the previous content) and `ZMAIL_LOG_DIR` overrides the directory for tests.
- Secrets must never be written to crash output, debug logs, or release notes.
- Dependency advisories are enforced in CI with `cargo audit` and `cargo deny`.

## Packaging And Signing

- GitHub Actions produces release archives for Linux, macOS, and Windows from the same tagged source.
- The current packaging target is the compiled `zmail-app` binary in a platform archive.
- Tagged releases additionally produce a Linux `.deb` package (via `cargo-deb`), a macOS `.app` bundle (ad-hoc signed), and a macOS DMG with an `/Applications` symlink, each attached with a `SHA256SUMS` manifest.
- The macOS `.app` bundle carries a generated application icon: `assets/icons/app-icon.svg` is rendered to `Zmail.icns` by `scripts/generate-app-icon.sh` (the same rsvg-convert + `sips` + `iconutil` pipeline Shuffleboard uses) and registered through `CFBundleIconFile`.
- The macOS DMG follows the Shuffleboard release layout (`scripts/create-dmg.sh`): a Finder icon view with large icons, the app and the `Applications` alias side by side. Its filename carries the version (`Zmail-<version>.dmg`) so downloads are identifiable on disk.
- When the org-wide Apple credentials are present, the macOS bundle is signed with the Developer ID Application certificate (hardened runtime, secure timestamp), notarized by Apple (`notarytool`, stapled and `spctl`-verified) before the DMG is created; without them the bundle falls back to an ad-hoc signature and the release never blocks. The org-wide secrets, shared with the other UnicornOps macOS apps (see `unicornops/shuffleboard`):
  - `APPLE_CERTIFICATE_BASE64` — the Developer ID Application certificate as base64-encoded P12.
  - `APPLE_CERTIFICATE_PASSWORD` — the P12 passphrase for the certificate above.
  - `APPLE_TEAM_ID` — the Apple Developer team identifier.
  - `APPLE_DEVELOPER_ID` and `APPLE_APP_PASSWORD` — the Apple ID and its app-specific password used by `notarytool`.
- Windows releases carry the zip archive and a WiX-built MSI installer (Program Files, Start-menu shortcut, `mailto:` scheme registration) produced by `cargo wix`; the executable embeds a VERSIONINFO resource (`crates/zmail-app/build.rs`) the MSI binds its product version against.
- Signing and notarization are opt-in, repository-controlled, and no-op until the secrets exist. The concrete secret inventory a maintainer adds to activate them:
  - `WINDOWS_SIGNING_CERT` — an Authenticode certificate as base64-encoded PFX. When present, the Windows release job signs the binary and the zip with `signtool` (RFC 3161 timestamping) before uploading.
  - `WINDOWS_SIGNING_PASSWORD` — the PFX passphrase for the certificate above.
  - `.deb` signing (pending work): a GPG key (`DEB_SIGNING_KEY` + `DEB_SIGNING_KEY_PASSWORD`) added to a repo signing step when apt hosting exists.
- No signing step may block the release when its secrets are absent, and no signing secret may ever be printed into build logs.
- No packaging step may introduce a background helper process that outlives the GUI binary.
- The release workflow can be exercised without cutting a release: dispatch **Release Please** manually with a tag or branch and the dry-run flag to build, sign, notarize, and produce the DMG without uploading anything.

## Release Process

- Versioning is bootstrapped at `0.0.1`.
- Release PRs and GitHub releases are managed by Google Release Please.
- Contributors should use Conventional Commit prefixes such as `feat:`, `fix:`, or `deps:` so Release Please can compute the next version and changelog.
- The release workflow builds release archives on all supported desktop platforms and uploads them to the GitHub release.
- A separate release-triggered workflow captures a Linux screenshot from the live mail harness and uploads the PNG to the same GitHub release without writing back to `main`.

### Changelog coverage for workspace changes

Release Please's `cargo-workspace` plugin counts only commits that touch the released package path (`crates/zmail-app`) when it builds the changelog, so features that live entirely in sub-crates (`zmail-store`, `zmail-sync`, `zmail-imap`, `zmail-jmap`, `zmail-ui`, and others) ship in the binary but are silently dropped from the generated changelog and never trigger a release PR at all (issue #67). Upstream has no `additional-paths` option for this, and the earlier root-release attempts (#22, #27, #33) showed the plugin synthesizes a second, unwanted release unit when the workspace is widened.

The gap is closed by the **Complete Release Changelog** job in the release workflow, which runs `scripts/render-release-pr.sh` after Release Please on every push to `main`:

1. When Release Please has a release PR open, the job regenerates the pending changelog section from every conventional `feat`/`fix` commit in the release window — app-crate and sub-crate alike — and pushes the completion to the release-please branch.
2. When no release PR is open but the window holds unreleased conventional commits, the job bootstraps the release PR exactly as Release Please would (version bump, manifest, `Cargo.lock`, changelog, `autorelease: pending` label).
3. Otherwise it is a no-op.

Merging the bootstrapped PR is recognized by Release Please through the label, which keeps tagging, release notes, and asset builds unchanged. The manual per-release audit is no longer required; reviewing the release PR before merging remains possible and encouraged.

The hand-audit ritual this automation replaced was required for the 0.3.0 and 0.4.0 releases (see the `chore: complete the ... changelog with workspace-wide changes` commits on the release-please branches) and the 0.7.1/0.8.0 hand-cut releases.
