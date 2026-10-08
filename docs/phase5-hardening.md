# Phase 5 Hardening

This document records the Phase 5 hardening work that is present in-repo today and calls out the remaining packaging gaps from [`PLAN.md`](../PLAN.md).

## Accessibility

- The GPUI shell keeps all primary flows keyboard reachable.
- Focus remains anchored inside the workspace after account creation, compose, sync, and outbox actions.
- Text-first reader rendering remains the default fallback for message content.
- Remote content stays blocked by default and attachments still require explicit user action.

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
- Secrets must never be written to crash output, debug logs, or release notes.
- Dependency advisories are enforced in CI with `cargo audit` and `cargo deny`.

## Packaging And Signing

- GitHub Actions produces release archives for Linux, macOS, and Windows from the same tagged source.
- The current packaging target is the compiled `zmail-app` binary in a platform archive.
- Tagged releases additionally produce a Linux `.deb` package (via `cargo-deb`) and a macOS `.app` bundle (ad-hoc signed), each attached with a `SHA256SUMS` manifest.
- Windows releases carry the zip archive and a WiX-built MSI installer (Program Files, Start-menu shortcut, `mailto:` scheme registration) produced by `cargo wix`; the executable embeds a VERSIONINFO resource (`crates/zmail-app/build.rs`) the MSI binds its product version against.
- Signing and notarization are opt-in, repository-controlled, and no-op until the secrets exist. The concrete secret inventory a maintainer adds to activate them:
  - `WINDOWS_SIGNING_CERT` — an Authenticode certificate as base64-encoded PFX. When present, the Windows release job signs the binary and the zip with `signtool` (RFC 3161 timestamping) before uploading.
  - `WINDOWS_SIGNING_PASSWORD` — the PFX passphrase for the certificate above.
  - macOS notarization (pending work, same pattern): `MACOS_SIGNING_CERTIFICATE` (base64 P12), `MACOS_SIGNING_CERTIFICATE_PASSWORD`, and the App Store Connect API key (`APPLE_ID`, `APPLE_ID_PASSWORD`, `APPLE_TEAM_ID`) for `notarytool`; today the `.app` ships ad-hoc signed.
  - `.deb` signing (pending work): a GPG key (`DEB_SIGNING_KEY` + `DEB_SIGNING_KEY_PASSWORD`) added to a repo signing step when apt hosting exists.
- No signing step may block the release when its secrets are absent, and no signing secret may ever be printed into build logs.
- No packaging step may introduce a background helper process that outlives the GUI binary.

## Release Process

- Versioning is bootstrapped at `0.0.1`.
- Release PRs and GitHub releases are managed by Google Release Please.
- Contributors should use Conventional Commit prefixes such as `feat:`, `fix:`, or `deps:` so Release Please can compute the next version and changelog.
- The release workflow builds release archives on all supported desktop platforms and uploads them to the GitHub release.
- A separate release-triggered workflow captures a Linux screenshot from the live mail harness and uploads the PNG to the same GitHub release without writing back to `main`.
