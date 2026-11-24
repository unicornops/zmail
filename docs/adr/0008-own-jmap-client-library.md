# ADR 0008: Zmail's Own JMAP Client Library

## Status

Accepted on November 23, 2026 (Rob's decision on #132). It supersedes [ADR 0001](./0001-jmap-client-strategy.md), whose escape hatch this is.

Progress:

- Step 1 (done): the `jmap-wire` crate with its protocol core, transport, session discovery and the Identity, EmailSubmission and `Email/set` types; `zmail-jmap`'s identities, submission and destroying of emails moved onto it.
- Step 2 (done): the reads. `jmap-wire` gained Mailbox, Thread and Email as typed, lenient data types with `/get`, `/changes`, `/query` and `/queryChanges`, typed result references, batching and gzip; `zmail-jmap`'s `Mailbox/get`, `Email/query`, `Email/get` and `Email/changes` moved onto it. See "Step 2: The Reads" below.
- Step 3 (done): the writes, import and blobs. `jmap-wire` gained `Email/set` in full with a typed patch, `Email/import`, `/copy`, `Mailbox/set`, and blob upload and download; `zmail-jmap`'s keyword and mailbox changes, the Sent copy's upload and import, and attachment downloads moved onto it, with two faults fixed on the way. See "Step 3: The Writes, Import and Blobs" below.
- Step 4 (done): push and sign-in. `jmap-wire` gained a streaming transport trait, the event source, JMAP over WebSocket and streamed blob downloads; `zmail-jmap`'s two push transports and its sign-in moved onto it; and `jmap-client` left the workspace, with its Dependabot ignore and its entry among the chrono wrappers in `deny.toml`. See "Step 4: Push, Sign-in and the End of `jmap-client`" below.

The move is finished: Zmail's JMAP is `jmap-wire` alone.

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
- **The transport trait is one method** (`execute`, with the timeout and size limit in the request). *Rejected:* making the client generic over `reqwest` middleware, or a `tower::Service` (a dependency and a concept more than the job needs). Streaming (event source) and WebSocket are not HTTP exchanges of this shape and are traits of their own since step 4. Step 3 kept blobs on this one method (see "Blobs are whole bodies" below).
- **Typed requests, lenient typed answers, raw JSON always available.** Records have every property optional and keep the ones the crate has no field for; lists and maps accept `null` (James writes it). Records to create are built property by property, so `null` can be told from "not set". *Rejected:* strict types that fail on a server's deviation, which is what failed with `jmap-client` on James.
- **A patch removes with `null`** (`PatchObject::remove`), as RFC 8620 section 5.3 says. `jmap-client` writes `false` for a removed keyword, which Fastmail is reported to refuse; step 3 uses this for keywords and mailboxes (`EmailUpdate`).
- **Dates are text.** `UtcDate` wraps the RFC 3339 string. *Rejected:* `jiff` in the public API (the workspace has it, but a library that picks a date crate for its users costs those who picked another) and `chrono` (banned in `deny.toml`). An optional `jiff` feature can come later without breaking anything.
- **Errors are typed** (`thiserror`), never `anyhow`; error type enums keep unknown values.
- **Nothing is retried** by the client, except once after a 401 when the credentials are a token source that can refresh. Which calls are safe to repeat is the caller's knowledge.
- **No new dependencies**: `serde`, `serde_json`, `url`, `base64`, `thiserror` and optionally `reqwest` and (from step 2) `flate2`, all already in the lock. Without the `reqwest` feature there is no async runtime either. Step 4 added `tokio` to the `reqwest` feature (for its timer; `reqwest` already brought it) and `tokio-tungstenite` with `futures-util` behind a feature that is off by default; all three were in the lock.

### How Zmail used both clients until step 4

*History: this is how steps 1 to 3 ran. Since step 4 there is one client.*

`zmail-jmap` signed in once. `jmap-client` fetched the session, and the `jmap-wire` client was given that same session (no second request), the same credentials, the same-host redirect rule and the same 10 second timeout. The calls that moved were built as typed `jmap-wire` requests; their answers were still read by the functions that read them before, so the errors users saw were worded as they had been. `wire_parity_tests.rs` in `zmail-jmap` built each moved call both ways and held the differences to `using`, the call id and the creation id.

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

## Step 3: The Writes, Import and Blobs

### What the library gained

Written from RFC 8620 sections 5.3, 5.4, 6.1 and 6.2 and RFC 8621 sections 2.5 and 4.6 to 4.8.

- **`Email/set` in full.** `NewEmail` has a setter for each property an email may be created with (mailboxes, keywords, the received date, the header fields with a property of their own, any other header field in a parsed form, and the body as `bodyStructure` or as `textBody`, `htmlBody` and `attachments`); `NewBodyPart` is a part whose content is a body value of the same call, a blob, or other parts. `EmailUpdate` is the typed patch for the two things of an email that can change. `ifInState` was there from step 1.
- **`Email/import`** (`EmailImport`, `EmailToImport`, `EmailImportResponse`), **`/copy` for any data type** (`CopyMethod`, `CopyObject`, `CopyResponse`, with `EmailCopy` for emails) and **`Mailbox/set`** (`NewMailbox`, `MailboxUpdate`, `onDestroyRemoveEmails`). `Email/copy` and `Mailbox/set` are in because they were nearly free and a general user needs them; Zmail calls neither. `Email/parse` and `Blob/copy` are still by name only.
- **`SetOutcome`**: what the answer to a `/set`, a `/copy` or an import says of one record: done (with what the server says of it), refused (with its `SetError`), or not named at all. `SetError` gained readers for what its types carry: `names_property` for `invalidProperties`, `existing_id` for `alreadyExists`, `blobs_not_found` for `blobNotFound`. Unknown SetError types are kept, as before.
- **Blobs**: `Client::upload`, `Client::download` and `Client::download_url`, with `BlobDownload`, `Blob` and `UploadedBlob`.
- **Fixtures** for the write answers: hand-written shapes of every outcome, and what the harness's James answered.

### Decisions within it

- **The typed patch has no way to write `false`.** `EmailUpdate` has `set_keyword` and `clear_keyword`, `add_to_mailbox` and `remove_from_mailbox`, `move_between`, and `replace_keywords` and `replace_mailboxes` for the whole sets. Clearing and removing write `null`; there is no method that takes a `bool`, because that parameter is how `"keywords/$seen": false` came to be sent. It also never patches a property both whole and in part, which RFC 8620 section 5.3 forbids (the later call wins), and it escapes ids and keywords as JSON Pointer parts. `Set::update` takes anything that converts to a `PatchObject`, so the general patch is still there for extensions. *Rejected:* `keyword(name, bool)` (the misuse it invites is the bug being fixed); a patch type per property (two more types for no more safety); and validating that an update leaves the email in a mailbox (only the server knows where it is).
- **An email to create is built property by property**, like the other records to create, with the body parts as their own builder. What RFC 8621 section 4.6 forbids combining (`bodyStructure` with the flat lists, the same header field twice, `Content-*` on the email) is documented and left to the server, which must answer `invalidProperties`. *Rejected:* a type-state builder that makes the wrong combinations uncompilable. It would be three or four builder types for errors a server reports clearly, and extensions would have to break out of it.
- **Created emails are records.** `Set<Email>`, `EmailImport` and `Email/copy` answer with lenient `EmailRecord`s (`id`, `blobId`, `threadId`, `size`, and anything else the server says), as step 2 found they should be.
- **`/copy` is called `CopyMethod`**, not `Copy`: a type named `Copy` in a crate's root shadows the standard trait for anyone who imports it.
- **Blobs are whole bodies, with limits of their own.** This was the open question from step 2: `HttpTransport::execute` returns a whole body, and attachments run to tens of megabytes. The two options were (a) whole-body upload and download with explicit limits apart from the API's, and (b) a streaming extension to the transport. Step 3 takes (a):
  - a download reads at most `max_download_bytes` (128 MiB by default) or the download's own `max_bytes`, counted after decompression, and fails with the `TooLarge` transport error over that instead of holding more;
  - an upload over the session's `maxSizeUpload` is refused before anything is sent (`Error::UploadTooLarge`);
  - both run under `blob_timeout` (five minutes by default), not the API timeout, and a download can set its own;
  - `Client::download_url` gives the URL for a caller who must stream a blob to disk with an HTTP client of their own.

  Why not streaming now: Zmail, the one user, hands each attachment to its store as one slice to encrypt (`BlobStore::write`, at most 25 MiB each, ADR 0005), so a stream would be collected into memory on the next line. A streaming trait designed for a download alone would also be designed without its harder user, the event source of step 4, which needs an idle timeout rather than a total one, no body limit, and reads that end only when the caller stops. Designing it once, for both, is step 4's job (see "What step 4 needs"). The cost of (a) is stated: a download is in memory once, and an upload twice while it is sent, because `HttpRequest` owns its body and the client keeps the bytes for a redirect or a refreshed token.
- **Blob requests are not asked for compressed.** An attachment is rarely compressible and a server that gzips a download changes its `Content-Length`. An answer that is gzip all the same is decompressed within the download's limit; one in an encoding the client cannot undo is an error, never handed on as if it were the blob.
- **Credentials go to the session's upload and download URLs, and no further by default.** RFC 8620 sections 6.1 and 6.2 make both requests authenticated ones, and a session may put those URLs on another host than its own (a separate domain for user content): the session is the server's own statement of where its endpoints are, exactly as for the API URL. A *redirect* from either is judged by the client's `RedirectPolicy` like any other, against the host of the blob URL: with the default policy the credentials never leave that host and a redirect elsewhere is refused. A download may opt into following such a redirect **without** credentials (`BlobDownload::follow_redirects_without_credentials`), for servers that answer with a signed link to a storage host; once left behind, the credentials do not come back on a later hop, even to a host that may have them. HTTPS to HTTP stays refused. *Rejected:* sending credentials along any redirect of a download (the storage host, or whoever answers in its place, would get them) and following uncredentialed by default (a redirect to an unexpected host is more often a misconfiguration or an attack than a design).
- **`Content-Type` is the caller's on upload**, and `{name}` and `{type}` are filled in the download URL (percent-encoded, as RFC 6570 level 1 says); the type asked for is also the request's `Accept`.

### What Zmail does with it

Keyword changes, moves, the Sent copy's upload and `Email/import`, and attachment downloads are built with `jmap-wire`. **`jmap-client` is left with push** (the event source's headers and URLs, the WebSocket connection with its heartbeat `Mailbox/get`) **and the session fetch at sign-in** with the `Deref` shim on `Connection`.

The move was pinned first, as in step 2: `write_characterisation_tests.rs` runs the write-back queue, a send and a sync with attachments against a server on loopback and compares what was sent, what was reported and what the server then holds with text written while the writes were still on `jmap-client`. Two faults that the text pinned are fixed on purpose:

1. **Clearing a keyword sent `false`.** Marking unread and unflagging now send `null`. A server that keeps to RFC 8620 section 5.3 refused the old form, and the email stayed read or flagged on the server.
2. **A move replaced the whole `mailboxIds` set**, so an email that was in other mailboxes too (labels) lost them. A move now names two mailboxes: the one the email leaves (`null`) and the one it joins (`true`), in one update. RFC 8620 section 5.3 applies the entries of a patch together, so an email in a single mailbox is never in none on the way. The whole set is still replaced in two cases: when the target is the trash or the junk mailbox, because RFC 8621 section 4.6 says deleting to the trash is putting the email there "and remove all other Mailbox ids" (an email left under a label would still show there, and be destroyed with the trash), and when the server has no mailbox with the source's name any more. A server that refuses the per-mailbox patch (`invalidPatch`, `invalidProperties` that does not point elsewhere, or James's `invalidArguments`) is sent the same change as the whole set, worked out from the mailboxes the email is in at that moment, so the other mailboxes are kept there too. Undoing an archive is a move back and restores the email to where it was; undoing a delete brings it back to the mailbox it was deleted from only, since the delete took it out of the others.

**What Apache James 3.9 does with the old and the new forms**, from CI's live harness (the captures are in `crates/jmap-wire/tests/fixtures/james-3.9`): it refuses `"keywords/$seen": false` with `invalidPatch` ("Keywords partial updates requires a JsBoolean(true) (set) or a JsNull (unset)") and takes `null`. So marking unread and unflagging had been failing against James as well, not only (as reported) against Fastmail; the harness had no test of either, and has one now. James takes a move as two mailbox patches, including out of the email's only mailbox and into another in one update, so the fallback to the whole set did not run there. One thing James does against RFC 8621 section 4.1.1: an update that takes an email out of its only mailbox and adds none is answered `updated`, and the email is destroyed. Zmail never sends that: every move adds its target in the same update, and where it does not know the source it replaces the set.

Zmail has no copy within an account. If it gains one, it is `EmailUpdate::add_to_mailbox`; `Email/copy` is for copying between accounts.

Everything else is held to what it was, with the differences listed in the pull request: blob URLs are percent-encoded (a blob id with a `/` or a space in it used to fail to download), blob requests say what they accept, four failures are worded by `jmap-wire`, an unknown SetError type is reported as itself instead of failing the whole answer, and blobs get 60 seconds instead of the 10 of an API call. Attachments keep their limit of 25 MiB, which is now also what a download reads at most: a larger answer is dropped without being held in memory first.

### What step 4 needed

*History: written at the end of step 3; step 4 did each of these.*

- **One streaming piece, for the event source, that downloads can then use.** A second trait beside `HttpTransport` (an optional capability of a transport, so a transport that only does exchanges still works for everything but push): open a request and get the status, the headers and the body as chunks. The client's `exchange` function already separates what it needs from a response to decide on redirects, the 401 retry and the credentials (the status and the headers) from the body, so the same loop can drive both kinds of transport. Its timeout must be an idle one, and it has no size limit. A `download_to` that writes chunks to a caller's sink can then be added on that trait without changing `download`.
- **The WebSocket binding** (RFC 8887) as a third trait: it carries the same `Request` and `Response` objects, which already serialise without the client.
- **Sign-in and discovery on `ClientBuilder::connect` and `discovery::session_url`**, then the `Deref` shim, `jmap_client_url`, the bearer token guard, `wording.rs`'s parity tests and `wire_parity_tests.rs` go with `jmap-client`, its Dependabot ignore and its `deny.toml` entry.
- **The account id** is still the one `jmap-client` picks (`default_account_id`); `jmap-wire`'s `primary_account` for mail must be shown to pick the same before the switch.

### Seen and not changed

- **Mailboxes are still named, not identified** (#512). The write-back queue names the source and target of a move as the mirror does, and Zmail resolves the names to ids with a `Mailbox/get` before it writes. Step 3 made the source's name matter as well as the target's: with two mailboxes of one name, a move may now name the wrong one as the source, in which case the email joins the target and stays where it was (the removal of a mailbox it is not in changes nothing). Before, it would have been moved correctly, and stripped of every other mailbox. Carrying mailbox ids in the queue, which #512 asks for, removes both.
- **A keyword change or a move of an email the server no longer has** is retried until it is dead-lettered, as before; only destroying treats `notFound` as done. With typed SetErrors this is now one `match` away, and belongs with the queue's retry rules.
- **A send refused for an invalid address is retried** (#460). The refusal's type is now a `SetErrorType` in the library, but the code that classifies it moved in step 1 and reads JSON; changing it belongs to #460.
- **The "unchanged" shortcut** (#513) is untouched.

## Step 4: Push, Sign-in and the End of `jmap-client`

### What the library gained

- **A streaming transport.** `StreamingTransport` is a second trait a transport may offer beside `HttpTransport`: `open` returns the status and the headers as soon as they are in, and a `BodyStream` whose `chunk` hands the body over piece by piece. `ReqwestTransport` implements it.
- **The event source** (RFC 8620 section 7.3), in `event_source`: a server-sent events parser, the request's variables (`types`, `closeafter`, `ping`) and `Last-Event-ID`, typed events, and a stream that knows when the server has gone quiet.
- **`StateChange`** (section 7.1), with the `pushState` of RFC 8887.
- **JMAP over WebSocket** (RFC 8887), in `websocket`: the messages as text, two traits for the socket, `Client::connect_websocket` and `WebSocketConnection`, and behind the `tungstenite` feature a transport on `tokio-tungstenite`.
- **Streamed blob downloads**: `Client::download_stream`, on the streaming trait. `Client::download` is unchanged.
- **`Credentials::check`**, `Session::websocket`, and the documentation of how an account is chosen.

### Decisions within it

- **One streaming trait, optional, with its own request type.** A transport that only does exchanges still works for everything but the event source and streamed downloads. `StreamRequest` has a `response_timeout` (connecting and waiting for the status and headers) and nothing else to bound it: no total timeout and no size limit, because a response that never ends would meet both. What bounds a stream is the idle time each `chunk` call is given, which the transport enforces (it has the timer; the protocol core has none), and the size of one line or one event of an event stream, which the parser enforces. *Rejected:* a second timeout field on `HttpRequest` and a flag for "no limit" (every transport would have to learn that a whole-body request may have neither, and `max_response_bytes` would stop meaning what it says); returning a `futures` `Stream` (a dependency in the public API of the protocol core, for one method); and an idle timeout fixed when the stream is opened (the event source learns the server's real ping interval from its first ping, after opening).
- **The redirect, 401 and credential rules are one loop for both kinds of transport.** They are decided on the status and headers alone, so the client's loop is generic over "something with a status and headers" and is given the function that sends one hop. A streamed response that is a redirect or a 401 is dropped unread. *Rejected:* a second copy of the loop for streams (two places to keep the rule for who sees the credentials).
- **A stream is never decompressed.** The event source and a streamed download ask for no compression, and an answer in a content encoding all the same is an error. Whole-body downloads still undo gzip. *Rejected:* streaming decompression (a decompressor's state machine and its own bound on expansion, for answers no server has been seen to send).
- **The server-sent events parser follows the specification, and bounds what it leaves unbounded.** All three line endings wherever the bytes are cut (a carriage return and its line feed may arrive in different pieces), comments (which servers send as keep-alives), `data` over several lines, `id` (ignored when it contains a NUL, reset by an empty one, and made the stream's last event id when an event is dispatched, even one without data), `retry` (digits only), a byte order mark, and invalid UTF-8 read with replacement characters. A line, or the data of one event, over 1 MiB is an error. Zmail's earlier parser, which this replaces, split on line feeds only and ignored `id` and `retry`.
- **Reconnecting is the caller's.** The stream reports its last event id and the server's `retry`, and `EventSource::last_event_id` sends the id back; nothing in the crate waits and tries again. A helper would need a timer, which the crate has only inside a transport, and a policy (how long, how often, when to give up, what a wake from sleep means) that belongs to the application: Zmail's lives in `zmail-sync` and did not change. The module documentation has a loop with backoff to copy. *Rejected:* a `reconnecting` wrapper with a backoff the caller configures (it would still not know about Zmail's wake signal, so Zmail would not use it, and it would be the only part of the crate that sleeps).
- **Silence is measured by the pings.** With pings asked for, a stream on which nothing at all arrives for the ping interval plus a grace (15 seconds unless set) fails with a timeout; the interval is the one asked for until the server's first ping says which it really uses (never taken as less than 5 seconds). With `ping=0` there is no check unless the caller sets an idle time.
- **`types` is written with its commas.** RFC 6570 level 1 would percent-encode the commas of `Email,Mailbox` and the `*`. Both are the syntax of the value RFC 8620 defines and are allowed in a query, and a server that splits the list before decoding it would see one unknown type; Zmail has always sent them literally, to James among others. The type names themselves are encoded.
- **The WebSocket is two traits, and the library is a feature.** `WebSocketTransport::connect` takes a handshake (URL, headers, subprotocol, timeout, message and frame limits) and returns a `WebSocket` that sends and receives text. The binding itself (the messages, the client's part of the handshake, `WebSocketConnection`) needs no feature and is tested without a network. `TungsteniteTransport` is behind `tungstenite`, off by default. *Rejected:* depending on `tokio-tungstenite` outright (every user of the crate would build a WebSocket library for a push transport most servers do not need), and leaving the default implementation to each user (Zmail needs one, and the handshake rules below are easy to get wrong).
- **What the WebSocket costs in crates.** `tokio-tungstenite` was in the lock only through `jmap-client`. Keeping WebSocket push keeps three crates that would otherwise have left with it: `tokio-tungstenite`, `tungstenite` and `sha1` 0.10 (everything else they need is shared with the rest of the tree). The alternative was to drop WebSocket push, which Zmail has, tests against James, and falls back to when a server's event source cannot be opened. Three small crates, behind a feature, is the cheaper side of that.
- **The WebSocket handshake is held to the HTTP side's rules.** The credentials go to the URL the session advertises, as they go to its API, upload and download URLs. A `ws` URL is refused when the session came over HTTPS, as a redirect from HTTPS to HTTP is. A handshake answered with a redirect fails: nothing says the credentials may follow, and the WebSocket libraries do not follow one either. A 401 gets one more attempt when the credentials can refresh; any other status is `Error::Http` with that status. The server must confirm the `jmap` subprotocol (RFC 8887 section 4.2). Messages are limited to the client's response size limit and frames to 16 MiB. TLS is rustls against the system's roots, as `jmap-client` had it.
- **A request-level error over the WebSocket is a message, not the end of the connection** (`WebSocketMessage::RequestError`, with the same `ProblemDetails` as an HTTP error status), since RFC 8887 answers one bad request with it and carries on. A message that is none of the three kinds the RFC allows is kept as `Unknown`. A message without `@type` is read by what it has.
- **Streamed downloads return a reader, not a writer.** `download_stream` returns a `BlobStream` whose `chunk` the caller writes wherever it likes. *Rejected:* `download_to(impl AsyncWrite)`: there is no `AsyncWrite` without choosing `tokio`'s or `futures`' for every user of the protocol core. Its limits are a stream's: the response must begin, and never fall silent for longer than, the client's timeout; there is no size limit unless the download sets one.
- **`PushSubscription` is not typed.** It is the third way to be pushed to (the server POSTs to a URL the client registers), needs a public endpoint a desktop client does not have, and is called by name like any other method.

### The account

`jmap-client`'s `default_account_id` was the first entry of `primaryAccounts` as its hash map happened to order it, and the order changed from one sign-in to the next. Tests written before the move (`sign_in_characterisation_tests.rs`) show what that gave, over forty sign-ins each:

| Session | `jmap-client` | Now |
| --- | --- | --- |
| One account, primary for everything | that account | the same |
| Several accounts, one primary for everything | that account | the same |
| Different primary accounts for mail and for submission | either, at random | mail's |
| Mail in one account; contacts, calendars and quota in another | either, at random | mail's |
| A primary account for contacts only | the contacts account | the account that holds mail |
| No primary accounts, one account with mail | the empty string | that account |
| No accounts | the empty string | the sign-in fails, saying so |

So on a server whose every primary account is the same one, which is every server Zmail is known to have been used with, nothing changes. On a server that names another account for any capability, Zmail used to sync whichever account the hash map put first, differently on different sign-ins; it now always uses the primary account for mail.

**Sending happens in the mail account**, identities included. An `EmailSubmission` names the email to send and the identity to send it as by ids of its own account (RFC 8621 section 7), and the email is the Sent copy Zmail has just imported into the mail account, so the submission and its identities cannot be in another. A server whose primary account for submission is not its primary account for mail is therefore used through the mail account for both; if that account cannot submit, the server says so (`accountNotSupportedByMethod`) and the send fails with that. Importing the copy into the submission account instead would put it where Zmail does not sync. No server with that arrangement has been seen.

### What Zmail does with it

- **Sign-in** is `discovery::session_url` (the well-known URL's redirects, followed without credentials) and `ClientBuilder::connect_with`, with redirects kept to the session's host, as before. The same-host rule, the three shapes of endpoint and the refusal of a session that moves to another host are pinned by `session_tests.rs` and `sign_in_characterisation_tests.rs`.
- **The event source** is `Client::event_source` with `types=Email,Mailbox`, `closeafter=no` and `ping=30`, on a client whose redirects stay on the event source's own host (as Zmail's own event source client had it). Which events mean mail changed did not change: a state change that touches Email, EmailDelivery, Mailbox or Thread of the account, or a `state` event that cannot be read.
- **The WebSocket** is `Client::connect_websocket` over `TungsteniteTransport`, with the same steps as before: a `Mailbox/get` of no ids repeated until the server answers (Apache James loses frames sent right after the upgrade), `WebSocketPushEnable`, one more request whose answer shows push is on, and then the same request as a heartbeat every 30 seconds.
- **Reconnecting, backoff, polling a server without push, and the wake signal** are in `zmail-sync` and were not touched: `watch_mail_changes` has the signature and the results it had.

`push_characterisation_tests.rs` ran both transports against a scripted server on loopback while they were still on `jmap-client`, and the text it pinned changed in the move only as follows.

On the wire:

- the session request and the event source carry `User-Agent: jmap-wire/0.1.0` where `jmap-client/0.4.1` was (API requests already did), the session request asks for `application/json` and gzip, and the lookup of a well-known URL asks for `application/json` where it asked for anything;
- the WebSocket handshake carries a `User-Agent`, which it did not;
- WebSocket request ids are `r0`, `r1`... where they were `0`, `1`..., and the heartbeat's call id is `c0` where it was `s0`.

In behaviour:

- **A pushed state change that names a data type from a later specification** used to fail to decode in `jmap-client`, and Zmail took every such failure for a change to mail, whichever account it was for. It is now read, and counts only if it touches mail in Zmail's account. A WebSocket message that is not JSON, or not one of the three kinds, is still taken for a change.
- **A server that accepts the event source connection and then sends nothing, not even headers,** used to be waited for without limit. It is now given 60 seconds (the connect timeout, a ping interval and the grace) and then treated as an event source that cannot be opened.
- **The WebSocket handshake** has the 10 second timeout of an API request, where it had none.
- **The bearer token rule** is `jmap-wire`'s: a token that is empty, or has a byte outside printable ASCII, is refused before anything is sent, with the same message as before. `jmap-client` built the header with an `unwrap`, so Zmail had a guard in front of it that refused what `reqwest` would not take as a header value; that guard let a tab and bytes above ASCII through, which no OAuth token has (RFC 6750). There is no `unwrap` on this path now: the token is checked where the header is built, and a transport is handed only header values that are ones.
- **An event source on another host than the session's** is unchanged (redirects are followed on its own host), by a client built for it.

In wording (the sentences a failed sign-in or a push failure is reported in):

- an answer to the session request that is not JSON, or not a session, reads "the JMAP server's session could not be read: ..." where it read "Parse error: ..."; a session without `capabilities` is no longer refused for that, only one without `apiUrl`;
- a session that redirects to another host reads "the JMAP server redirected to ..., which was refused: the redirect policy does not allow it" where it read "Transport error: error following redirect for url (...)";
- a timeout or a failed connection names the URL and the cause, where it read "Transport error: error sending request for url (...)" with `jmap-client`'s URL trick showing (`...#/.well-known/jmap`);
- a refused WebSocket handshake reads "Server failed: 401 Unauthorized" where it read "WebSockets error: HTTP error: 401 Unauthorized", and a refused redirect of the event source says where it led instead of "answered HTTP 307 Temporary Redirect".

Wrong credentials (401), a wrong URL (404), a server error and problem details read exactly as they did, and the add-account flow takes the same answers for refused credentials.

### What left the dependency tree

Nine crates left `Cargo.lock`: `jmap-client` 0.4.1, `reqwest` 0.13.5 (the second `reqwest`), `async-stream`, `async-stream-impl`, `maybe-async`, `h2`, `tokio-util`, and `wasm-streams` 0.4.2 and 0.5.0. Three that only `jmap-client` brought stay for the WebSocket transport: `tokio-tungstenite`, `tungstenite` and `sha1` 0.10. `chrono` is still in the tree through `async-imap` and the pinned GPUI (`gpui`, `scheduler`, `zlog`), so its advisory ignore stays, with one wrapper fewer.

### What was learnt

- **Pinning behaviour as text before each move paid for itself four times.** Every difference in this record was found by a diff of a text file, not by reading code, and two real faults (step 3) and one silent one (the account, above) were only visible that way.
- **A lenient decoder removes workarounds.** Zmail treated every WebSocket message `jmap-client` could not decode as "something changed", because a new data type made the whole message undecodable. With a decoder that keeps what it does not know, the workaround's reason went away.
- **Timeouts are per kind of exchange.** An API call, a blob, a stream and a handshake each needed their own, and each was first written with someone else's: blobs under the API timeout (step 3), the event source with no limit on the wait for headers (here).
- **A hash map is not a choice.** "The default account" was an iteration order.
- **The cost of a dependency includes what it hides.** The URL trick, the panic guard, the `false` for `null`, the capabilities in `using` and the account were each a behaviour of Zmail that nobody had decided.

### What making it a project of its own needs

Not done, and not to be done without a decision; a checklist for when it is.

1. **Licence.** Choose it (MIT OR Apache-2.0 is what is expected of a library). Add `LICENSE-MIT` and `LICENSE-APACHE`, set `license` in `Cargo.toml` instead of `license.workspace`, and have everyone who has contributed to `crates/jmap-wire` agree to the new licence (`git log -- crates/jmap-wire` is the list). Confirm once more, by review, that nothing in the crate comes from another JMAP crate.
2. **Its own dependency versions.** Replace every `workspace = true` with a version and exactly the features the crate uses: `tokio` with `time` alone for the `reqwest` feature and `net` for `tungstenite`; `reqwest` with `rustls-tls` and no `json` or `blocking`; `futures-util` with `sink`; `flate2`; `tokio-tungstenite` with `rustls-tls-native-roots`. Today the workspace's wider feature sets hide a missing one.
3. **Features.** Decide the default set (`reqwest` and `gzip` today), whether `tungstenite` should verify against the same roots as `reqwest` (it uses the system's, `reqwest` the bundled ones), and whether to take a rustls configuration directly so that users need not install a crypto provider (today `TungsteniteTransport::new` relies on rustls's process-wide one, and rustls panics at the first `wss` connection when it cannot pick one; Zmail installs one, and the type's documentation says so). Consider a `jiff` feature for dates.
4. **MSRV policy.** It is the workspace's (1.98) because the code uses what that allows (`let` chains, `async` closures in a doctest). Decide a policy (the last N stable releases, say), set `rust-version`, and test it in CI.
5. **CI of its own.** Format, Clippy and tests for the default features, none, each alone and all; `cargo doc` with warnings as errors; `cargo deny`; the three operating systems, since the loopback tests exercise sockets; and the MSRV. The live test against a real server (Apache James in Zmail's harness) has to move with it or be replaced: without it, nothing checks the crate against a server.
6. **Fixtures from other servers.** Recorded answers from Fastmail, Stalwart and Cyrus, one directory each, and recorded push from any server (none exists yet, James included).
7. **README and docs.rs.** A README that does not send the reader to Zmail; `package.metadata.docs.rs` with `all-features`; `doc(cfg)` on what is behind a feature; a `CONTRIBUTING` with the written-from-the-RFCs rule.
8. **Version policy.** Semantic versioning from 0.1.0; a changelog per release (the crate's `CHANGELOG.md` is already its own); what `#[non_exhaustive]` promises; and a decision on the name before it is reserved.
9. **The repository move, with history.** `git filter-repo --subdirectory-filter crates/jmap-wire` (or `git subtree split`) keeps the crate's commits; the squash commits of steps 1 to 4 also touch Zmail and will read oddly, so link this record from the new repository. Zmail then depends on it by version, and `zmail-jmap`'s characterisation tests, which read fixtures from the crate's directory by path, need the fixtures published in the package or copied.
10. **What is still missing from the library** for a user who is not Zmail: `PushSubscription`, `Email/parse`, `SearchSnippet/get`, `VacationResponse` and `Blob/copy` as typed calls; streamed uploads; a reconnecting event source for those who want one; and capabilities beyond mail.

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

Made after step 3:

- **Blobs get 60 seconds** in Zmail (step 3's change from the 10 seconds of an API call) stands.
- **The crate still stays in this repository**, to become a distinct project later, with the licence change then.

Still open, for the split: the licence itself, a version policy, and the crate's own dependency versions and features in place of the workspace's. "What making it a project of its own needs" above is the list.

## Consequences

- Until step 4, Zmail carried two JMAP clients and both `reqwest` versions. Since step 4 it carries one of each, and three crates for the WebSocket transport.
- Zmail owns the decoding of `Email/get` from step 2 on. CI tests only Apache James; the fixture tests in `crates/jmap-wire/tests/fixtures` take recorded answers from any server, one directory each, and Zmail's characterisation tests replay any directory that has a mailbox list and a sync answer. Recordings from Fastmail and Stalwart are still wanted: until then, what protects those users is that the decoding does not fail on a shape it has not seen.
- A JMAP change in Zmail may now be a change in two crates, one of which must stay free of Zmail.
- Zmail is now the only thing between its users and a JMAP server's quirks: there is no upstream to wait for, and none to blame. Apache James is the one server CI runs; other servers are untested until someone records their answers or runs Zmail against them.
