# Zmail Plan

## Goal

Build a fast, secure, standards-compliant, cross-platform desktop email client in Rust with a classic three-pane layout, using GPUI for the application shell and a Zed-compatible theming layer.

This plan assumes:

- The repository starts empty.
- UI responsiveness is a hard requirement.
- Very large mailboxes are normal, not an edge case.
- All dependencies must be compatible with a GPL release.

## Product Principles

- Never block the UI thread on network, disk, parsing, or indexing work.
- Be offline-first: the local store is the primary read path.
- Optimize for large accounts: hundreds of folders, hundreds of thousands of messages, and deep history.
- Default to secure behavior: no silent trust downgrades, no remote content auto-loading, no unsafe HTML rendering.
- Keep protocol and storage layers modular so IMAP, JMAP, and future account types can evolve independently.
- Reuse Zed and GPUI where it is technically and legally sound, but pin upstream inputs tightly to avoid churn.

## Confirmed Decisions

- License: `GPL-3.0` for the project, represented in Rust metadata as `GPL-3.0-only`.
  - Reason: this keeps the project aligned with Zed's GPL distribution model and remains compatible with common Apache-2.0 and MIT Rust dependencies.
- Platform targets for MVP: macOS, Linux, Windows.
- Protocols from day one: generic IMAP + SMTP submission + JMAP, with JMAP treated as equal-priority MVP scope.
- Multiple identities are a baseline requirement from day one, not a later enhancement.
- OS keychain integration and normal disk encryption are sufficient for MVP local-data protection.
- Email encryption features such as OpenPGP and S/MIME are post-MVP.
- Local storage: SQLite metadata store plus content-addressed message body and attachment blobs on disk.
- Caching policy is configurable per account from day one.
- The default cache policy is a performance-first profile: full metadata mirror, prefetch for recent and visible message bodies, and lazy download for older bodies and attachments.
- Search: SQLite FTS5 first. Only add a separate search engine if benchmarks show it is necessary.
- Theming: support Zed theme JSON files unchanged, using the Zed schema and a thin pinned compatibility layer in Zmail.
- HTML rendering is security-first: prefer a constrained sanitized renderer, and only adopt a webview path if an explicit sandboxing spike proves it meets or exceeds that security bar.

## Architectural Direction

### Workspace layout

Recommended Rust workspace layout:

- `crates/zmail-app`
  - Application entry point, window setup, command routing, dependency wiring.
- `crates/zmail-ui`
  - GPUI views, pane layout, reusable widgets, keyboard navigation, focus model.
- `crates/zmail-theme`
  - Zed theme compatibility, theme loading, semantic token mapping, icon and spacing tokens.
- `crates/zmail-core`
  - Domain types: account, identity, mailbox, thread, message, draft, sync state, events.
- `crates/zmail-store`
  - SQLite schema, queries, migrations, caches, blob storage, FTS indexes.
- `crates/zmail-sync`
  - Background orchestration, job scheduler, account supervisors, retry policy, sync state machine.
- `crates/zmail-imap`
  - IMAP protocol adapter and capability handling.
- `crates/zmail-smtp`
  - SMTP submission and outbox delivery.
- `crates/zmail-jmap`
  - JMAP protocol adapter, capability handling, sync strategy, and JMAP submission support where available.
- `crates/zmail-auth`
  - OAuth2 flows, token refresh, account onboarding helpers.
- `crates/zmail-autodiscover`
  - Account autodiscovery from an email address: the domain's autoconfig and `.well-known/jmap`, ISPDB with consent, hostname guesses, and DNS SRV/MX once a resolver is chosen.
- `crates/zmail-security`
  - Secret storage abstraction, TLS policy, HTML sanitization, attachment safety policy.
- `crates/zmail-index`
  - Search indexing and query execution if this grows beyond simple store concerns.
- `crates/zmail-test-support`
  - Fixtures, protocol test harnesses, mailbox generators, benchmark datasets.

### Runtime model

- GPUI owns the UI thread and rendering lifecycle.
- Async services run on a dedicated Tokio runtime integrated with GPUI.
- Each account gets an isolated supervisor that owns:
  - connection lifecycle
  - sync jobs
  - push or IDLE listeners
  - retry and backoff state
  - send queue interaction
- The UI never talks to servers directly.
- The UI reads local state and emits commands.
- Background services update the store and publish coarse-grained events back to the UI.

### State flow

- Command from UI -> domain action -> async job or local mutation.
- Async job writes results into the store.
- Store change notifications invalidate or refresh visible view models.
- UI components hold lightweight state only:
  - selection
  - sort mode
  - visible query
  - scroll position
  - draft editor state
- Server-derived state belongs in the store, not in widgets.

## UI Plan

### Main layout

The default window is a traditional three-pane layout:

- Left pane: accounts, folders, smart folders, tags or labels.
- Center pane: thread list or message list for the selected mailbox.
- Right pane: message reader or conversation view.

Additional views:

- Account setup flow
- Search results view
- Compose window or detachable compose pane
- Settings and identity management
- Attachment viewer handoff dialog

### UI performance rules

- Use virtualized or uniform-height lists for all mailbox and thread views.
- Never bind list rendering to message body hydration.
- Message body fetches and attachment discovery happen incrementally.
- Expensive derived data must be precomputed or cached in the store.
- Scroll and selection state must survive background sync churn.
- UI updates from sync must be coalesced to avoid repaint storms.

### Reader behavior

- Render plain text immediately when available.
- Render sanitized HTML through a constrained renderer by default.
- Block remote images and remote styles by default.
- Strip script execution entirely.
- Only adopt a webview-backed HTML path if an explicit spike proves we can sandbox it tightly enough to preserve the project's secure-by-default posture.

## Data and Storage Plan

### Storage model

Use an offline-first local mirror:

- SQLite stores metadata, indexes, mailbox structure, flags, sync checkpoints, drafts, and UI caches.
- Multiple identities per account are stored as first-class records with per-identity transport and signature settings.
- Each account has an explicit cache policy from day one.
- Raw bodies and attachments live in a blob store on disk keyed by content hash.
- Keep body summaries and preview text in SQLite for fast list rendering.
- Persist normalized recipient and threading data for local search and sort.

### Core tables

Initial schema should cover:

- accounts
- identities
- mailboxes
- mailbox_state
- messages
- message_mailboxes
- message_bodies
- attachments
- threads
- participants
- flags
- labels
- sync_cursors
- outbox_jobs
- search_documents

### Large-mailbox strategy

- Initial sync fetches mailbox tree, envelope data, flags, and thread metadata before full body download.
- The default cache policy mirrors all metadata locally, prefetches message bodies for visible and recent mail, and downloads older bodies lazily.
- Cache policy is configurable per account so users can choose more aggressive offline caching where it improves their workflow.
- Attachments remain metadata-only until requested.
- Use paged local queries for lists; never hold complete mailbox contents in memory.
- Support cache eviction and configurable disk budgets for downloaded bodies and attachments.
- Run migrations and compaction in background jobs with resumable checkpoints.

### Search

- Start with SQLite FTS5 for subject, sender, recipients, normalized preview text, and optionally downloaded body text.
- Keep search local-first for responsiveness.
- Add server-side search only as an optional augmentation, not as the main path.

## Protocol Plan

### MVP

- IMAP for sync and mailbox operations.
- SMTP submission for sending mail.
- JMAP for sync and mailbox operations, plus `EmailSubmission` where supported by the provider.
- Treat standards-compliant JMAP servers as in-scope for MVP rather than narrowing to a small provider list.
- OAuth2 for major providers where possible, with password or app-password support for generic accounts if the server allows it.

### IMAP capability targets

Design for capability negotiation rather than assuming a perfect server. Prioritize:

- STARTTLS or implicit TLS
- SASL authentication
- IDLE
- UIDPLUS
- MOVE
- CONDSTORE
- QRESYNC
- LIST-STATUS
- SPECIAL-USE
- NAMESPACE

### Post-MVP or parallel spike

- Server rules and vacation settings
- Calendar or contacts only if they serve mail workflows directly

## Security Plan

### Hard requirements

- Secrets stored in the OS credential store, never in plain text config.
- TLS verification on by default with no insecure bypass in release builds.
- Remote content blocked by default.
- HTML sanitized before rendering.
- Attachments opened only through explicit user action.
- Sensitive logs redacted by default.
- Crash reports opt-in only, if they exist at all.

### Security features by phase

- MVP
  - OS keychain integration
  - strict TLS policy
  - sanitized message rendering
  - attachment quarantine flow
  - dependency vulnerability and license scanning
- Later
  - optional encryption at rest for local caches
  - OpenPGP and or S/MIME
  - phishing and lookalike domain warnings
  - certificate pinning or TOFU only if there is a clear product need

## Theming Plan

### Zed integration approach

Treat Zed compatibility as a first-class requirement:

- Support unchanged Zed theme files via the public Zed theme schema.
- Pin to a specific upstream Zed commit for any vendored or mirrored theme code.
- Keep the compatibility layer narrow:
  - parse theme family files
  - map Zed style keys to Zmail semantic roles
  - fill any mail-specific gaps with local semantic tokens
- Avoid depending on a moving `main` branch at runtime.

### Chosen implementation

Chosen long-term approach:

- vendor the minimum theme compatibility layer into `crates/zmail-theme`
- record the exact upstream Zed commit used
- keep the compatibility layer narrow enough that unchanged Zed theme files continue to work without pulling broad Zed workspace coupling into Zmail

## Dependency and Licensing Policy

### Rules

- Every dependency must have a known SPDX license expression.
- Only add dependencies that are compatible with `GPL-3.0-only`.
- Prefer MIT, Apache-2.0, BSD, ISC, Unicode, and similarly compatible licenses.
- Reject source-available but non-open-source licenses.
- Reject AGPL dependencies unless there is a deliberate reason to accept network-copyleft obligations.
- Pin critical git dependencies to exact revisions.

### Tooling

- Add `cargo-about` for license inventory and notice generation.
- Add `cargo-deny` for license policy, advisories, duplicate versions, and bans.
- Add `cargo-audit` or equivalent advisory scanning in CI.

### Candidate crates to spike in Phase 0

These are candidates, not final commitments:

- UI/runtime: `gpui`, `gpui_tokio` from the Zed codebase
- async runtime: `tokio`
- serialization: `serde`, `serde_json`
- store: `rusqlite`
- mail parsing: `mail-parser`
- mail building: `mail-builder`
- SMTP submission: `lettre` or `mail-send`
- IMAP: `async-imap` or equivalent maintained async IMAP client
- JMAP: a maintained Rust JMAP client or an in-tree implementation if the available ecosystem is too weak
- auth: `oauth2`
- secret storage: `keyring`
- TLS: `rustls`

Every one of these must pass a license and maintenance review before adoption.

## Quality Plan

### Test strategy

- Unit tests for parsing, threading, sync merge logic, migrations, and security policy.
- Integration tests against deterministic fixtures plus Docker-backed live IMAP and SMTP harnesses.
- Golden test corpus for malformed MIME, strange encodings, large threads, and broken servers.
- UI smoke tests for core flows and keyboard navigation.
- Protocol harness requirements:
  - start with a single repo-owned command, preferably `docker compose up`
  - expose a seeded IMAP account and SMTP submission path ready for automated tests
  - support send-then-receive round-trip assertions so the client is always tested against real protocol servers, not only mocks
- Container images and helper tooling used for the harness must be compatible with the project's GPL-3.0-only policy for repository dependencies and distribution practices.
- Benchmarks for:
  - opening a mailbox with 100k local messages
  - scrolling a large thread list
  - indexing imported mail
  - switching folders during active sync

### Performance budgets

- No visible UI freeze during sync or send operations.
- Mailbox switching should come from local state and feel immediate once the mailbox is indexed locally.
- Thread list scrolling should remain smooth under continuous background sync.
- Search should return an initial page from the local index quickly enough to feel interactive.

## Delivery Phases

### Phase 0: Feasibility and foundation

- Create the Rust workspace.
- Pin GPUI and confirm cross-platform build strategy.
- Prove Tokio and GPUI integration.
- Vendor the narrow Zed theme compatibility layer and validate unchanged Zed theme loading.
- Decide whether JMAP uses a third-party Rust client crate or an in-tree implementation.
- Define the initial license allowlist and CI policy.
- Build a synthetic large-mailbox benchmark harness.

Exit criteria:

- A minimal GPUI app runs on the primary development platform.
- Theme loading strategy is chosen and documented.
- CI can fail on license or advisory violations.

### Phase 1: Local-first shell

- Implement the main window and three-pane skeleton.
- Build the store schema and migration system.
- Seed the UI from mock data.
- Add virtualized list components and selection model.

Exit criteria:

- The UI can browse a large synthetic mailbox from the local store without network access.

### Phase 2: Account onboarding and sync

- Add account setup.
- Implement IMAP connectivity, mailbox discovery, and incremental sync.
- Implement JMAP account discovery and incremental sync.
- Add identity discovery and configuration as part of account onboarding.
- Persist sync checkpoints and failure state.
- Add background job supervision and reconnect logic.

Exit criteria:

- Real IMAP and JMAP accounts can sync mailboxes and browse locally cached metadata without blocking the UI.

### Phase 2b: Live protocol harness

- Add a repo-owned Docker Compose test harness for disposable IMAP and SMTP servers.
- Seed the harness with deterministic accounts, credentials, mailboxes, and sample messages so tests start from a known state.
- Add test helpers and app-level integration tests that boot the harness, sync mail through IMAP, submit mail through SMTP, and verify end-to-end round trips.
- Ensure the harness is ready for local development and CI use with documented startup, teardown, and reset flows.
- Keep the harness simple and standards-oriented so it validates that Zmail continues to speak to real mail servers during future development.

Exit criteria:

- A contributor can start the local IMAP and SMTP harness from the repo and run automated tests that prove Zmail can receive, sync, and submit mail against real protocol servers.

### Phase 3: Reader and mailbox operations

- Add message hydration, body rendering, thread view, flags, move, delete, archive, and search.
- Add safe HTML rendering and remote content controls.
- Add optimistic local updates where safe.

Exit criteria:

- The user can browse, read, flag, move, and search mail from a real account with acceptable latency.

### Phase 4: Compose and send

- Compose UI
- draft autosave
- multiple identities and signatures
- SMTP submission and retry queue
- JMAP `EmailSubmission` path for JMAP-native accounts where supported
- attachment add and remove flow

Exit criteria:

- The user can compose, save drafts, and send mail reliably with offline retry behavior.

### Phase 5: Hardening and packaging

- accessibility pass
- performance tuning
- crash and error reporting policy
- installers and signing
- release documentation

Exit criteria:

- MVP is ready for external testing on all supported platforms.

### Phase 6: Post-MVP

- encryption at rest
- OpenPGP and or S/MIME
- account import and export
- richer smart folders
- adaptive caching and sync policy for low-power and slow-connection conditions
- rules and automation

## Risks and Mitigations

- GPUI and Zed internals may change quickly.
  - Mitigation: pin commits, vendor narrow slices, avoid broad workspace coupling.
- IMAP server behavior is inconsistent across providers.
  - Mitigation: capability negotiation, fixture servers, provider compatibility matrix.
- HTML email rendering is a large security and correctness problem.
  - Mitigation: MVP uses a constrained sanitized renderer, not a general browser engine.
- Large mailboxes can overwhelm naive stores and list models.
  - Mitigation: local-first paging, virtualization, incremental hydration, early benchmarks.
- Licensing drift is easy in Rust dependency graphs.
  - Mitigation: automated CI gates before merge.

## Remaining Questions

No blocking questions remain for foundation work.

Current scope assumptions:

- unified inbox and unified outbox are not required for MVP
- multiple accounts are supported, each with multiple identities where configured
- account-level caching policy ships in MVP, while adaptive policy changes for low-power or slow-connection conditions are post-MVP

## References

- Zed repository and licensing: <https://github.com/zed-industries/zed>
- GPUI overview and examples: <https://www.gpui.rs/>
- GPUI documentation index: <https://www.gpui.rs/#examples>
- Zed theme schema: <https://zed.dev/schema/themes/v0.2.0.json>
- Zed settings reference for theme selection behavior: <https://zed.dev/docs/reference/all-settings>
