# Phase 5 Hardening

This document records the Phase 5 hardening work that is present in-repo today and calls out the remaining packaging gaps from [`PLAN.md`](../PLAN.md).

## Accessibility

Zmail is not yet usable without a pointer or with a screen reader. This section records what is true today, so nobody relies on support that does not exist. The work to close the gaps is in the UI plan (`UI_PLAN.md`): Phase 1 (actions, key bindings, per-pane focus, a real text input) and Phase 8 (screen readers, text scaling, reduced motion, high contrast).

### Keyboard

- There are no key bindings. The shell defines no GPUI actions or `KeyBinding`s, has no per-pane focus, and has no Tab order.
- The whole workspace has a single focus handle. Every key press goes to whichever text field was clicked last: the search field, a compose field, or an account-setup field. This holds even after clicking a thread or a mailbox.
- Enter submits single-line fields: it runs the search, adds the typed attachment path, and saves the account from the last account-setup fields. Nothing else responds to the keyboard.
- Choosing an account, mailbox, thread, draft or outbox job, and every toolbar, reader and compose button, needs a pointer. So the primary flows (read, reply, send, set up an account) are **not** keyboard reachable.
- Text fields are drawn labels with an inserted cursor, not an editor. There is no selection, copy, cut, undo, key repeat or IME. Paste uses GPUI's platform modifier, which is Cmd on macOS but not Ctrl on Linux or Windows. The cursor is a byte index, so editing next to a non-ASCII character can panic.

### Screen readers

- The pinned GPUI (a Zed stable release, see `docs/upstream/zed.md`) ships AccessKit, and Zmail starts it with accessibility enabled. Zmail's views do not set roles or labels yet, so screen readers still announce almost nothing in the window. That work is UI plan Phase 8.

### Visual

- The theme follows the system light/dark appearance until the toolbar toggle is used; the choice is not persisted yet.
- Dates in lists are relative ("10:42", "Yesterday"), with the full timestamp in a tooltip. The tooltip needs a pointer to hover; there is no keyboard equivalent yet.
- Text size follows GPUI's fixed 16px root size. There is no text scaling, high-contrast mode or reduced-motion handling.
- Status messages dismiss themselves after four seconds, so they can be missed. Errors stay until dismissed with the banner's close button, which needs a pointer.

### Content

- The reader renders message bodies as text, and HTML is converted to text before display.
- Remote content stays blocked by default, and attachments are only saved on explicit user action.

## Performance

- Network sync and outbox delivery stay on the dedicated Tokio background runtime in `zmail-sync`.
- The shell only schedules one delayed refresh at a time while background work is active.
- Local browsing, mailbox switching, and search continue to read from the local mirror rather than from network paths.
- Phase 5 adds tests that assert sync and outbox scheduling return immediately instead of waiting on slow background work.

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
