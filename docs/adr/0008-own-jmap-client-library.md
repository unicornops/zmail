# ADR 0008: Zmail's Own JMAP Client Library

## Status

Accepted on November 23, 2026 (Rob's decision on #132). It supersedes [ADR 0001](./0001-jmap-client-strategy.md), whose escape hatch this is.

Progress:

- Step 1 (done): the `jmap-wire` crate with its protocol core, transport, session discovery and the Identity, EmailSubmission and `Email/set` types; `zmail-jmap`'s identities, submission and destroying of emails moved onto it.
- Step 2: the reads (`Mailbox/get`, `Email/query`, `Email/get`, `Email/changes`).
- Step 3: the writes and import (`Email/set` keyword and mailbox changes, upload and `Email/import`, download).
- Step 4: WebSocket push; then `jmap-client` goes, with its Dependabot ignore and its entry among the chrono wrappers in `deny.toml`.

## Context

ADR 0001 put a third-party crate, `jmap-client`, behind `crates/zmail-jmap`, "with an explicit escape hatch to replace it if protocol coverage or maintenance becomes insufficient". Both have happened (#132):

- From 0.4.2, `jmap-client` puts every capability it knows (contacts, calendars, Sieve, quota, principals and more) in each request's `using`. RFC 8620 section 3.6.1 has a server reject a request that names a capability it does not support, and Apache James does. Zmail has been pinned to `=0.4.1` since.
- The fix was offered upstream on 2026-11-06 and has had no answer; the repository takes pull requests from collaborators only, the maintainer has said the crate will be rewritten, and 0.4.4 came out on 2026-11-23 without the fix.
- Zmail already worked around the crate in several places: a URL trick to make it fetch the session from the URL it is given, a guard against a panic on a bearer token that is not a header value, and identities, submission and destroying emails built with its builders but read as plain JSON because its typed answers refuse what some servers send.
- It brings eight crates nothing else in Zmail uses, a second `reqwest` (0.13) among them, and is one of the dependencies that still pull in `chrono`.

Two other JMAP crates exist (`io-jmap`, `jmap-base-client`). Both were first released in 2026 and still break their API often; either would mean the same rewrite of the adapter plus a young dependency to follow.

## Decision

Zmail writes its own JMAP client and moves onto it in four steps, keeping `main` releasable at each. The client is a library others can use, not a module private to Zmail: a workspace crate, `crates/jmap-wire`, that depends on no `zmail-*` crate and has no Zmail in its API.

### Layers

1. **Protocol core**, independent of any HTTP library: the request and response envelopes (RFC 8620 sections 3.3 and 3.4), method calls with call ids, result references (section 3.7), the Session object (section 2), and typed errors for all three levels: problem details (section 3.6.1, RFC 7807), method errors (section 3.6.2) and `SetError`s (section 5.3).
2. **Typed calls**: `Get` and `Set` generic over a data type, and the data types themselves in `jmap_wire::mail`. A call by name with JSON arguments (`Request::call_raw`) sits in the same request as typed calls, so a missing type never blocks a user.
3. **Client and transport**: `Client` over a small `HttpTransport` trait (one HTTP exchange, no redirects), with credentials, a redirect policy, a timeout and a response size limit. The default transport is on the `reqwest` 0.12 the workspace already has, behind the `reqwest` feature (on by default).

### Decisions within it

- **`using` is computed per request**: Core, then the capability of each call in the request (a typed call states its own; a call by name is looked up by its data type for the types of RFC 8620 and 8621), then anything the caller adds. `Request::set_capabilities` overrides the lot. Core is always listed: the envelope is Core's, and every example in the RFCs lists it. An `EmailSubmission/set` adds the mail capability only when it uses `onSuccessUpdateEmail` or `onSuccessDestroyEmail`, whose implicit `Email/set` is a mail method. *Rejected:* listing what the session advertises (it sends capabilities the request does not use, which is the bug in another form), and leaving `using` to the caller (every caller would have to get it right).
- **The client follows redirects, not the transport.** The policy (`none`, `same_host`, `same_origin`, `hosts`, or a function) is then the same whatever the HTTP library, and is the one place that decides which hosts see the credentials. HTTPS to HTTP is always refused, as is a redirect that would turn an API POST into a GET. Zmail's own rule (only the session's host) is one policy among them. *Rejected:* a transport that follows redirects by its own policy (each implementation would have to reimplement the rule, and a caller could not tell whether it had).
- **The transport trait is one method** (`execute`, with the timeout and size limit in the request). *Rejected:* making the client generic over `reqwest` middleware, or a `tower::Service` (a dependency and a concept more than the job needs). Streaming (event source) and WebSocket are not HTTP exchanges of this shape and will be traits of their own in step 4.
- **Typed requests, lenient typed answers, raw JSON always available.** Records have every property optional and keep the ones the crate has no field for; lists and maps accept `null` (James writes it). Records to create are built property by property, so `null` can be told from "not set". *Rejected:* strict types that fail on a server's deviation, which is what failed with `jmap-client` on James.
- **A patch removes with `null`** (`PatchObject::remove`), as RFC 8620 section 5.3 says. `jmap-client` writes `false` for a removed keyword, which Fastmail is reported to refuse; step 3 uses this for keywords.
- **Dates are text.** `UtcDate` wraps the RFC 3339 string. *Rejected:* `jiff` in the public API (the workspace has it, but a library that picks a date crate for its users costs those who picked another) and `chrono` (banned in `deny.toml`). An optional `jiff` feature can come later without breaking anything.
- **Errors are typed** (`thiserror`), never `anyhow`; error type enums keep unknown values.
- **Nothing is retried** by the client, except once after a 401 when the credentials are a token source that can refresh. Which calls are safe to repeat is the caller's knowledge.
- **No new dependencies**: `serde`, `serde_json`, `url`, `base64`, `thiserror` and optionally `reqwest`, all already in the lock. Without the `reqwest` feature there is no async runtime either.

### How Zmail uses both clients until step 4

`zmail-jmap` signs in once. `jmap-client` fetches the session, and the `jmap-wire` client is given that same session (no second request), the same credentials, the same-host redirect rule and the same 10 second timeout. The calls that moved are built as typed `jmap-wire` requests; their answers are still read by the functions that read them before, so the errors users see are worded as they were. `wire_parity_tests.rs` in `zmail-jmap` builds each moved call both ways and holds the differences to `using`, the call id and the creation id.

## Open Decisions (Rob's)

- **Licence.** The crate is GPL-3.0-only like the workspace. Libraries meant for others are usually MIT OR Apache-2.0. The crate was written from the RFCs and Zmail's own code, with nothing copied from another JMAP crate, so the choice is free.
- **Name.** `jmap-wire` was free on crates.io on 2026-11-23 and claims nothing about being the JMAP crate. It is not reserved.
- **Publishing.** Not published (`publish = false`). Publishing needs the licence decision, explicit dependency versions and features in the crate's own manifest instead of the workspace's, a version policy, and ideally steps 2 and 3 first so the first release covers reading mail.
- **Its own repository.** It can stay here while Zmail is its only user and the API moves; moving it out later costs a `git filter-repo` and a dependency line.

## Consequences

- Until step 4, Zmail carries two JMAP clients and both `reqwest` versions.
- Zmail owns the decoding of `Email/get` from step 2 on. CI tests only Apache James; the fixture tests in `crates/jmap-wire/tests/fixtures` take recorded answers from any server, one directory each, and need recordings from Fastmail and Stalwart before step 2 is trusted there.
- A JMAP change in Zmail may now be a change in two crates, one of which must stay free of Zmail.
