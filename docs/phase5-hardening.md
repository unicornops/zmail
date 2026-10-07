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

## Crash And Error Reporting Policy

- Zmail does not upload crash reports or telemetry automatically.
- Errors stay local to the terminal, local logs, and GitHub issues opened by users.
- Secrets must never be written to crash output, debug logs, or release notes.
- Dependency advisories are enforced in CI with `cargo audit` and `cargo deny`.

## Packaging And Signing

- GitHub Actions produces release archives for Linux, macOS, and Windows from the same tagged source.
- The current packaging target is the compiled `zmail-app` binary in a platform archive.
- Tagged releases additionally produce a Linux `.deb` package (via `cargo-deb`) and a macOS `.app` bundle (ad-hoc signed), each attached with a `SHA256SUMS` manifest.
- Windows releases still ship the zip archive; an MSI installer is pending release work.
- Developer ID signing and notarization are not configured yet and remain opt-in and repository-controlled. Release automation should only be extended with platform signing secrets when those secrets are available and the signing identity is stable.
- No packaging step may introduce a background helper process that outlives the GUI binary.

## Release Process

- Versioning is bootstrapped at `0.0.1`.
- Release PRs and GitHub releases are managed by Google Release Please.
- Contributors should use Conventional Commit prefixes such as `feat:`, `fix:`, or `deps:` so Release Please can compute the next version and changelog.
- The release workflow builds release archives on all supported desktop platforms and uploads them to the GitHub release.
- A separate release-triggered workflow captures a Linux screenshot from the live mail harness and uploads the PNG to the same GitHub release without writing back to `main`.
