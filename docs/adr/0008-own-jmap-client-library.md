# ADR 0008: Zmail's Own JMAP Client Library

## Status

Accepted on November 23, 2026 (Rob's decision on #132). It supersedes [ADR 0001](./0001-jmap-client-strategy.md), whose escape hatch this is.

Progress:

- Step 1 (done): the `jmap-wire` crate with its protocol core, transport, session discovery and the Identity, EmailSubmission and `Email/set` types; `zmail-jmap`'s identities, submission and destroying of emails moved onto it.
- Step 2 (done): the reads. `jmap-wire` gained Mailbox, Thread and Email as typed, lenient data types with `/get`, `/changes`, `/query` and `/queryChanges`, typed result references, batching and gzip; `zmail-jmap`'s `Mailbox/get`, `Email/query`, `Email/get` and `Email/changes` moved onto it. See "Step 2: The Reads" below.
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
- **No new dependencies**: `serde`, `serde_json`, `url`, `base64`, `thiserror` and optionally `reqwest` and (from step 2) `flate2`, all already in the lock. Without the `reqwest` feature there is no async runtime either.

### How Zmail uses both clients until step 4

`zmail-jmap` signs in once. `jmap-client` fetches the session, and the `jmap-wire` client is given that same session (no second request), the same credentials, the same-host redirect rule and the same 10 second timeout. The calls that moved are built as typed `jmap-wire` requests; their answers are still read by the functions that read them before, so the errors users see are worded as they were. `wire_parity_tests.rs` in `zmail-jmap` builds each moved call both ways and holds the differences to `using`, the call id and the creation id.

## Step 2: The Reads

### What the library gained

- **Data types**: `Mailbox` (every property of RFC 8621 section 2, with `myRights` and roles, unknown roles kept), `Thread` and `Email` for reading: metadata, the convenience header properties, any header field in any parsed form (`header:Name:asForm:all`), `bodyStructure`, `textBody`, `htmlBody`, `attachments`, and `bodyValues` with the `Email/get` arguments that control them.
- **Methods, for any data type**: `Changes`, `Query` (filter operators, comparators, position or anchor, limit, `calculateTotal`) and `QueryChanges`, beside step 1's `Get` and `Set`. `Mailbox/changes`, `Mailbox/query`, `Thread/get` and the `queryChanges` methods came almost free once the generic methods existed, so they are in; `Mailbox/set` is not (nothing reads through it).
- **Request-size helpers**: `IdBatches` and `Client::get_in_batches`, and the session's `maxObjectsInGet`, `maxCallsInRequest` and `maxSizeRequest` through `Session::core`.
- **Typed result references** (`IdsReference<O>`), and a check of every reference before a request is sent.
- **gzip** in the client, and `Response::take`.

### Decisions within it

- **Records never fail on an object.** A record (`MailboxRecord`, `EmailRecord`, `EmailBodyPart`, `EmailBodyValue`, `ThreadRecord`) has every property optional; a property that is missing or `null` is `None`; a property whose value is not of the RFC's type is `None` as well, and its JSON is kept in the record's `extra`, where properties the crate has no field for are kept too. Only something that is not a JSON object at all is a decoding error. This is the main risk of the step, because a strict decoder turns one odd value from one server into a failed sync, which is what happened with `jmap-client` and James. A test decodes four thousand generated objects of arbitrary shape, and the fixtures are also written back and compared with what was sent. *Rejected:* strict `serde` derives (one bad value fails the answer); defaulting an odd value silently (the caller cannot tell "absent" from "unreadable"; with `extra` it can); and telling `null` from missing in the type (a `/get` caller knows which properties it asked for, and servers do not agree on which of the two means "none").
- **Header fields are read by form.** A fetched `header:` property stays in `extra` under the name the server gave it and is read with `header::<AsText>("Subject")` or `header_all::<AsRaw>("Received")`, where the form is a type and the value's type follows from it. Field names match without regard to case; the last-instance and `:all` forms stand in for each other; instances the server could not parse (`null`) are left out. *Rejected:* one enum of every form and multiplicity (fourteen variants to match on for a value whose form the caller chose) and guessing the form from the JSON (an array of strings may be message ids, URLs or raw values).
- **Filters are built, not written as JSON**: `Filter<C>` is a condition of the data type or an `AND`/`OR`/`NOT` of filters; `EmailFilterCondition` has a setter for each property of RFC 8621 section 4.4.1 and one for extensions. The operator constructors take filters, and a condition becomes one with `into()`: taking "anything that converts" made the type of a nested filter ambiguous.
- **Batching is in the library as a small state machine** (`IdBatches`: start at `maxObjectsInGet`, halve when the server answers `requestTooLarge`, a `limit` problem for `maxSizeRequest`, or an answer over the client's size limit; never grow again), with one loop over it for the plain case. Zmail keeps its own loop, because it downloads attachments between batches, and its own cap of 250. The decision to halve is made on the typed error (`Error::is_request_too_large`). *Rejected:* leaving it to every user (the hidden limit of James took Zmail a bug report to find, #389) and hiding it inside every `/get` (a caller with a result reference, or its own pacing, could not opt out).
- **Result references are typed and checked.** `query.ids()`, `changes.created()`, `emails.thread_ids()` and `threads.email_ids()` return an `IdsReference` of the data type the ids are of, which the `ids_from` of that type's `/get` takes; the ids of another type do not compile. A reference written by hand still converts, unchecked as to type. Every reference in a request must name an earlier call of that request, by id and method, or the request is refused before it is sent. *Rejected:* a lifetime tying a reference to its request (it spreads through every signature for a mistake the check finds at once).
- **Compression is the client's, not the transport's.** Step 1 had dropped `Accept-Encoding: gzip`. Turning on `reqwest`'s own `gzip` feature would have turned it on for every `reqwest` client in the workspace (features are unified), including ones that are not ours to change, and would have done nothing for other transports. Instead the client asks for gzip and inflates the answer itself with `flate2`, behind the `gzip` feature (on by default). `flate2` was already in the lock through GPUI and rPGP, so the lock gains one dependency edge and no crate. The response size limit is applied to the decompressed bytes as they are produced: a kilobyte that expands to a megabyte is refused at the limit without being held. Only gzip: it is what every server offers, and brotli or zstd would be new crates.
- **The 64 MiB limit stays**, with a way round it. `jmap-client` had no limit at all, and `Email/get` with body values can legitimately be large: 250 newsletters of 300 kB are over it. So an answer over the limit makes `IdBatches` halve the batch like `requestTooLarge` does, and only a single email whose answer alone is over 64 MiB cannot be read (it fails with an error that says so). Raising the default instead would only move the point at which a hostile or broken server can exhaust memory.
- **`HttpRequest::new`** settles step 1's open question: a transport written outside the crate can now build a request, for its own tests and for exchanges of its own.

### What Zmail does with it

`Mailbox/get`, `Email/query` (sync paging, server search, finding an interrupted send's copy), `Email/get` (the sync, the light fetch after a change, the header backfill, the Message-ID lookup) and `Email/changes` are built as typed `jmap-wire` calls and decoded by it; `GetBatches` is a thin layer over `IdBatches`. What still calls `jmap-client`: keyword and mailbox changes, upload and `Email/import`, blob download, both push transports (the event source takes its headers and URLs from it), and the session fetch at sign-in.

The move was pinned first: `read_characterisation_tests.rs` replays recorded answers (captures from the harness's James, and hand-written shapes) through a full sync, an incremental sync, the header backfill, a search and a move, and compares what Zmail would store with text written while the reads were still on `jmap-client`. Those files did not change in the move. `wire_parity_tests.rs` compares each request with the one `jmap-client` built.

What a server's refusal reads like to the user was kept (`wording.rs`): method errors, problem details and HTTP error statuses are worded as `jmap-client` worded them. Failures of the exchange itself are worded by `jmap-wire`.

### What was learnt, for steps 3 and 4

- **The lenient record is the pattern for `Email/import` and `Email/set` answers too**: the `created` records of step 3 should be records, not maps.
- **Keyword changes must remove with `null`.** Zmail's sync treats `"$seen": false` in an answer as "not seen" on purpose (servers must not send it, some do), but what it *sends* to clear a keyword is still `jmap-client`'s `false`, which RFC 8620 section 5.3 does not allow for a patch. `PatchObject::remove` is there for step 3.
- **Blob download needs a streaming exchange**, or at least its own size limit: `HttpTransport::execute` returns a whole body, and attachments are up to 25 MiB each in Zmail. `HttpRequest::new` lets Zmail make the exchange over the client's transport; whether `jmap-wire` gets a `download` of its own or a streaming trait is step 3's decision, together with the upload.
- **Responses are parsed twice over** (bytes to JSON, JSON to records). `Response::take` avoids the third copy; if large mailboxes show the second one in profiles, decoding straight from bytes into typed responses is possible behind the same API.
- **The session is still fetched by `jmap-client`** and converted. Step 4 moves sign-in onto `ClientBuilder::connect` and `discovery::session_url`, which step 1 already tested against the same URL shapes.
- **Nothing here has been run against Fastmail, Stalwart or Cyrus.** The fixture layout takes a directory per server; the live test that recorded James can be pointed at any server with a test account.

## Decisions (Rob's)

Made after step 1:

- **The crate stays in this repository for now.** The plan is to make it a distinct project later.
- **Its licence changes when that happens.** It is GPL-3.0-only today, like the workspace. A permissive licence such as MIT OR Apache-2.0 is what is expected of a library; which one is decided at the split.
- **What that asks of the work meanwhile**, so that relicensing stays possible and the split stays cheap:
  - the crate is written only from the RFCs and this project's own code. Nothing is copied from `jmap-client` or any other JMAP crate; reading how they behave, for comparison, is fine;
  - it depends on no `zmail-*` crate and has no Zmail in its public API;
  - it stays `publish = false`, and the name is not reserved on crates.io.
- **The name `jmap-wire` stands for now.**
- **The CI step that tests the crate on its own stays** (`cargo test -p jmap-wire --no-default-features`, and its documentation with warnings as errors).

Still open, for the split: the licence itself, a version policy, and the crate's own dependency versions and features in place of the workspace's.

## Consequences

- Until step 4, Zmail carries two JMAP clients and both `reqwest` versions.
- Zmail owns the decoding of `Email/get` from step 2 on. CI tests only Apache James; the fixture tests in `crates/jmap-wire/tests/fixtures` take recorded answers from any server, one directory each, and Zmail's characterisation tests replay any directory that has a mailbox list and a sync answer. Recordings from Fastmail and Stalwart are still wanted: until then, what protects those users is that the decoding does not fail on a shape it has not seen.
- A JMAP change in Zmail may now be a change in two crates, one of which must stay free of Zmail.
