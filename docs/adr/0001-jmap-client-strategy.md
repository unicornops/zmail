# ADR 0001: JMAP Client Strategy

## Status

Accepted on March 20, 2026.

## Context

Zmail needs first-class JMAP support in MVP, including mailbox sync and submission support where servers expose `EmailSubmission`. The Rust ecosystem has a few JMAP libraries, but the project needs a path that keeps protocol logic isolated behind `crates/zmail-jmap`.

## Decision

Use a third-party JMAP client crate behind a thin adapter boundary in `crates/zmail-jmap`, with an explicit escape hatch to replace it if protocol coverage or maintenance becomes insufficient.

Phase 0 chooses `jmap-client` from Stalwart Labs as the initial candidate to spike first because:

- it documents support for JMAP Core (RFC 8620), JMAP Mail (RFC 8621), and JMAP over WebSocket (RFC 8887)
- it includes mail, mailbox, thread, identity, and `email_submission` modules
- it is published under Apache-2.0 or MIT, which is compatible with Zmail's GPL-3.0-only distribution

## Consequences

- `crates/zmail-jmap` owns Zmail-specific account modeling and retry policy; the external crate stays hidden behind internal traits.
- If the spike reveals protocol gaps, weak observability, or unsuitable async behavior, Zmail can replace the adapter internals without destabilizing the rest of the workspace.
- The decision is about the first implementation path, not a permanent lock-in.
