# ADR 0002: Message Encryption Protocol

## Status

Proposed on October 15, 2026 — awaiting acceptance (issue #92).

## Context

PLAN.md defers email encryption to post-MVP, and issue #92 asks for the
first supported protocol to be chosen before implementation. Two
candidates exist:

- **OpenPGP** (RFC 4880 and successors): decentralized key discovery
  (Autocrypt, WKD, key servers), an established Rust implementation in
  Sequoia (LGPL-2.0-or-later with GPL-friendly linking for GPL projects)
  plus lighter alternatives such as `rpgp` (Apache-2.0/MIT).
- **S/MIME** (RFC 5751): X.509 certificate chains anchored to
  established CAs, common in managed/corporate deployments, but
  certificate handling and enrollment is heavier tooling in Rust and the
  trust model forces CA intermediaries into the trust path.

User-side demands: mail clients that interoperate with Gmail and
Fastmail users rarely have S/MIME certificates; the OpenPGP ecosystem has
the Autocrypt standard explicitly designed for opportunistic in-band key
exchange, which fits Zmail's local-first, no-account-server stance.

## Decision

OpenPGP is the first supported protocol, implemented on top of a
dedicated OpenPGP crate behind a thin adapter boundary in a new
`crates/zmail-pgp`, in this order:

1. **Read side first**: decrypt and verify incoming PGP/MIME messages in
   the reader, with explicit states for decrypted, verified, tampered,
   unknown-sender, and undecryptable, matching the information-first
   security posture of the reader (issue #91's warnings are the template).
2. **Per-identity private keys** live exclusively in the secret store
   (`zmail-security`'s named slots, extended with a per-identity
   OpenPGP slot); key material never touches the SQLite store.
3. **Write side second**: sign by default once a signing key exists;
   encrypt when every recipient has a discoverable key. Autocrypt headers
   (RFC 4880bis / Autocrypt 1.x) are the primary discovery path, WKD
   (RFC 8636) the second; key-server discovery stays out of scope.
4. **Trust management UI** is its own phase after the read side ships.

S/MIME stays explicitly deferred: re-evaluate once OpenPGP decrypt /
verify / sign / encrypt works end to end and demand for S/MIME exists.

## Consequences

- The adapter boundary keeps the OpenPGP crate replaceable; `rpgp` is
  the lighter fallback if Sequoia's build complexity (C tooling) hurts
  the three-platform release pipeline. The spike starts with `rpgp` for
  its pure-Rust, permissively licensed implementation and evaluates
  Sequoia only if `rpgp` coverage falls short.
- Cross-protocol mail (S/MIME-only correspondents) remains plaintext for
  now, and the reader must not pretend otherwise.
- License review is required for the chosen crate before adoption
  (cargo-deny gates GPL-3.0-only compatibility in CI).
- The phased plan means the issue's end-to-end acceptance criteria
  (decrypt/verify in, encrypt/sign out, at least one identity) close
  across two implementation phases, tracked as follow-ups of #92.
