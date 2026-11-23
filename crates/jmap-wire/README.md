# jmap-wire

A JMAP client library for Rust ([RFC 8620](https://www.rfc-editor.org/rfc/rfc8620) core, [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621) mail), in layers that can each be used without the ones above:

1. **A protocol core** with no HTTP library in it: request and response envelopes, method calls with call ids, result references, the Session object, and typed errors at all three levels (request, method and `/set`).
2. **Typed calls**: `Get` and `Set` for any data type, and the mail data types implemented so far. Anything else is called by name with JSON arguments, in the same request as typed calls.
3. **A client** over a small transport trait, with Basic and Bearer authentication (and a hook for refreshing a token), a redirect policy the caller chooses, a timeout and a response size limit. The default transport uses `reqwest` with rustls.

It is developed in the [Zmail](https://github.com/unicornops/zmail) repository, which is its first user, but has nothing of Zmail in it. **It is not published on crates.io yet**, and its API will change while the rest of JMAP Mail is added.

## Why another JMAP crate

A JMAP request lists the capabilities it uses in `using`, and a server must reject a request that names one it does not support (RFC 8620 section 3.6.1). A client that lists every capability it knows about therefore fails against any server that implements only some of them. This crate works `using` out from the method calls in each request: Core, plus what those calls need, and nothing else.

```rust
use jmap_wire::{Request, capability};
use serde_json::json;

let mut request = Request::new();
request.call_raw("Mailbox/get", json!({"accountId": "u1"}));
request.call_raw("Email/query", json!({"accountId": "u1"}));
assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
```

A call for a data type the crate has no table entry for says what it needs with `Request::add_capability`, and `Request::set_capabilities` replaces the computed list outright.

## Example

```rust,no_run
use jmap_wire::{Client, Credentials, Get, Request, capability, mail::Identity};
use serde_json::json;

async fn example() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .credentials(Credentials::bearer("an-api-token"))
        .connect("https://api.example.com/jmap/session".parse()?)
        .await?;
    let account = client.primary_account(capability::MAIL)?;

    // One request, two calls: one typed, one by name fed by a result
    // reference to another.
    let mut request = Request::new();
    let identities = request.call(Get::<Identity>::new(account))?;
    let query = request.call_raw(
        "Email/query",
        json!({"accountId": account, "filter": {"hasKeyword": "$draft"}}),
    );
    let drafts = request.call_raw(
        "Email/get",
        json!({"accountId": account, "#ids": query.result_reference("/ids")}),
    );

    let response = client.send(&request).await?;
    for identity in response.get(&identities)?.list {
        println!("{:?}", identity.email);
    }
    println!("{}", response.arguments(&drafts)?["list"]);
    Ok(())
}
```

## What is implemented

| Area | State |
| --- | --- |
| Session object, with capabilities and properties the crate does not know kept as JSON | done |
| Finding the session URL from a bare origin or `/.well-known/jmap`, following its redirects without credentials | done |
| Request and response envelopes, call ids, result references, `createdIds` | done |
| Problem details (RFC 7807), method errors, `SetError`s, unknown types kept | done |
| `Foo/get` and `Foo/set` for any data type, `PatchObject` | done |
| `Identity/get`, `Identity/set` | done |
| `EmailSubmission/get`, `EmailSubmission/set` with `onSuccessUpdateEmail` and `onSuccessDestroyEmail` | done |
| `Email/set` | destroy and patches; no typed properties for creating yet |
| `Mailbox`, `Thread`, `Email/get`, `/query`, `/changes`, `/queryChanges`, `/copy`, `Email/import`, `SearchSnippet`, `VacationResponse` | by name only (`Request::call_raw`) |
| Blob upload and download | URL templates only |
| Push: event source, WebSocket (RFC 8887), `PushSubscription` | URL template only |

It has been run against Apache James 3.9 (through Zmail's live tests). It has not been run against Fastmail, Stalwart or Cyrus.

## Design notes

- **Redirects.** A transport never follows redirects; the client does, by a `RedirectPolicy`. A redirect that is followed gets the credentials, so the policy says which hosts may see them. The default is the same host only. HTTPS to HTTP is always refused, and so is a redirect that would turn an API POST into a GET.
- **Secrets.** `Debug` output of credentials and requests has no header values, bodies or query strings; error messages have URLs without their query strings.
- **Limits.** Each exchange has a timeout (30 seconds by default) and a largest response size (64 MiB by default; 4 MiB for the session).
- **Nothing is sent twice** by the client on its own, except once after a 401 when the credentials are a token source that can refresh.
- **Dates** stay the RFC 3339 text the server sent (`mail::UtcDate`); the crate depends on no date library.
- **Unknown data** is kept, not dropped: capability objects, extra properties on records and errors, and error types from later specifications.
- **Removing with a patch** is `PatchObject::remove`, which writes `null` as RFC 8620 section 5.3 says (not `false`).

## Features

- `reqwest` (default): `ReqwestTransport` and `ClientBuilder::connect`. Without it the crate depends only on `serde`, `serde_json`, `url`, `base64` and `thiserror`, and needs no async runtime: implement `HttpTransport` for your own HTTP client.

## Minimum Rust version

The workspace's: Rust 1.98.

## Licence

GPL-3.0-only for now, as the rest of the Zmail workspace. Relicensing this crate under MIT OR Apache-2.0 before it is published is an open decision of the project owner; it was written from the RFCs and Zmail's own code so that the choice is free.
