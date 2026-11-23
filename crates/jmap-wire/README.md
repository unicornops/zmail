# jmap-wire

A JMAP client library for Rust ([RFC 8620](https://www.rfc-editor.org/rfc/rfc8620) core, [RFC 8621](https://www.rfc-editor.org/rfc/rfc8621) mail), in layers that can each be used without the ones above:

1. **A protocol core** with no HTTP library in it: request and response envelopes, method calls with call ids, result references, the Session object, and typed errors at all three levels (request, method and `/set`).
2. **Typed calls**: `Get`, `Set`, `Changes`, `Query` and `QueryChanges` for any data type, and the mail data types implemented so far, read into records that tolerate what servers leave out, null or add. Anything else is called by name with JSON arguments, in the same request as typed calls.
3. **A client** over a small transport trait, with Basic and Bearer authentication (and a hook for refreshing a token), a redirect policy the caller chooses, a timeout, a response size limit and gzip. The default transport uses `reqwest` with rustls.

It is developed in the [Zmail](https://github.com/unicornops/zmail) repository, which is its first user, but has nothing of Zmail in it. **It is not published on crates.io yet**, and its API will change while the rest of JMAP Mail is added. The plan is for it to become a project of its own later.

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

Reading mail the idiomatic JMAP way: a query and a get of what it finds, in one request (RFC 8620 section 3.7).

```rust,no_run
use jmap_wire::{
    Client, Comparator, Credentials, Get, Query, Request, capability,
    mail::{Email, EmailFilterCondition, EmailProperty, Mailbox, Role},
};

async fn example() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::builder()
        .credentials(Credentials::bearer("an-api-token"))
        .connect("https://api.example.com/jmap/session".parse()?)
        .await?;
    let account = client.primary_account(capability::MAIL)?;

    // One call in a request of its own.
    let mailboxes = client.call(Get::<Mailbox>::new(account)).await?.list;
    let inbox = mailboxes
        .iter()
        .find(|mailbox| mailbox.role == Some(Role::Inbox))
        .and_then(|mailbox| mailbox.id.as_deref())
        .ok_or("no inbox")?;

    // Two calls in one request, the second fed by the first.
    let mut request = Request::new();
    let query = request.call(
        Query::<Email>::new(account)
            .filter(EmailFilterCondition::new().in_mailbox(inbox).not_keyword("$seen"))
            .sort([Comparator::new("receivedAt").descending()])
            .limit(20),
    )?;
    let emails = request.call(
        Get::<Email>::new(account)
            .ids_from(query.ids())
            .properties([EmailProperty::From, EmailProperty::Subject, EmailProperty::ReceivedAt]),
    )?;

    let mut response = client.send(&request).await?;
    for email in response.take(&emails)?.list {
        // Every property of a record is an Option: a server returns what
        // was asked for, and may leave out or null what a message lacks.
        println!("{:?} {:?}", email.received_at, email.subject);
    }
    Ok(())
}
```

Changing mail: keywords and mailboxes are patched one member at a time, a message that exists as text is uploaded and imported, and an attachment is downloaded by its blob id.

```rust,no_run
use jmap_wire::{
    BlobDownload, Client, HttpTransport, Set, SetOutcome,
    mail::{Email, EmailImport, EmailToImport, EmailUpdate},
};

async fn example(
    client: &Client<impl HttpTransport>,
    account: &str,
    message: Vec<u8>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Unread, and from the inbox to the archive, leaving the email in
    // any other mailbox it is in. A keyword is cleared with null.
    let mut set = Set::<Email>::new(account);
    set.update(
        "M1",
        EmailUpdate::new().clear_keyword("$seen").move_between("inbox-id", "archive-id"),
    );
    let answer = client.call(set).await?;
    match answer.outcome_of_update("M1") {
        SetOutcome::Done(_) => println!("changed"),
        SetOutcome::Refused(error) => println!("refused: {error}"),
        SetOutcome::Unanswered => println!("the server did not say"),
    }

    // A message as RFC 5322 text: upload it, then import the blob.
    let blob = client.upload(account, "message/rfc822", message).await?;
    let mut import = EmailImport::new(account);
    let creation_id = import.email(EmailToImport::new(blob.blob_id, ["sent-id"]).keywords(["$seen"]));
    let imported = client.call(import).await?;
    println!("{:?}", imported.outcome_of(&creation_id));

    // An attachment, never more than 25 MiB of it.
    let download = BlobDownload::new(account, "blob-id-of-the-part")
        .name("report.pdf")
        .max_bytes(25 * 1024 * 1024);
    let attachment = client.download(&download).await?;
    println!("{} bytes", attachment.bytes.len());
    Ok(())
}
```

Anything the crate has no type for is called by name, in the same request as typed calls:

```rust
use jmap_wire::{Get, Request, capability, mail::Email};
use serde_json::json;

let mut request = Request::new();
let found = request.call_raw(
    "Email/query",
    json!({"accountId": "u1", "filter": {"text": "invoice"}, "limit": 5}),
);
request.call(Get::<Email>::new("u1").ids_from(found.result_reference("/ids")))?;
let snippets = request.call_raw(
    "SearchSnippet/get",
    json!({
        "accountId": "u1",
        "filter": {"text": "invoice"},
        "#emailIds": found.result_reference("/ids")
    }),
);
assert_eq!(request.using(), [capability::CORE, capability::MAIL]);
# let _ = snippets;
# Ok::<(), jmap_wire::Error>(())
```

## What is implemented

| Area | State |
| --- | --- |
| Session object, with capabilities and properties the crate does not know kept as JSON | done |
| Finding the session URL from a bare origin or `/.well-known/jmap`, following its redirects without credentials | done |
| Request and response envelopes, call ids, result references, `createdIds` | done |
| Problem details (RFC 7807), method errors, `SetError`s, unknown types kept | done |
| `Foo/get`, `Foo/set`, `Foo/changes`, `Foo/query` and `Foo/queryChanges` for any data type, `PatchObject`, filter operators and comparators | done |
| Result references, typed by the data type whose ids they resolve to, and checked before a request is sent | done |
| `Mailbox/get`, `/changes`, `/query`, `/queryChanges`, with `myRights` and roles | done |
| `Thread/get`, `/changes` | done |
| `Email/get`: metadata, header fields in every parsed form (`header:Name:asForm:all`), `bodyStructure`, `textBody`, `htmlBody`, `attachments`, `bodyValues` and the arguments that control them | done |
| `Email/query` with every filter condition and sort, `Email/changes`, `Email/queryChanges` | done |
| Splitting a long id list by `maxObjectsInGet`, halving on `requestTooLarge` (`IdBatches`, `Client::get_in_batches`) | done |
| `Identity/get`, `Identity/set` | done |
| `EmailSubmission/get`, `EmailSubmission/set` with `onSuccessUpdateEmail` and `onSuccessDestroyEmail` | done |
| `Email/set`: creating (every settable property, body parts by blob or inline value), keyword and mailbox changes with a typed patch, destroying, `ifInState` | done |
| `Email/import`, `Email/copy` (`/copy` for any data type), `Mailbox/set` | done |
| What a `/set`, `/copy` or import answer says of each record, with the `SetError`s and what they carry | done |
| Blob upload and download, whole bodies with their own size limit and timeout | done |
| `Email/parse`, `Blob/copy`, `SearchSnippet`, `VacationResponse` | by name only (`Request::call_raw`) |
| Push: event source, WebSocket (RFC 8887), `PushSubscription` | URL template only |

It has been run against Apache James 3.9 (through Zmail's live tests, reading and writing), and its decoding is tested against answers recorded from James and the examples of the RFCs (`tests/fixtures`). It has not been run against Fastmail, Stalwart or Cyrus: recorded answers from those are wanted, one directory each.

## Design notes

- **Redirects.** A transport never follows redirects; the client does, by a `RedirectPolicy`. A redirect that is followed gets the credentials, so the policy says which hosts may see them. The default is the same host only. HTTPS to HTTP is always refused, and so is a redirect that would turn an API POST into a GET.
- **Secrets.** `Debug` output of credentials and requests has no header values, bodies or query strings; error messages have URLs without their query strings.
- **Lenient records.** What a server returns is read into records whose properties are all `Option`s. A property that was not asked for, that the server left out or that is `null` is `None`; one whose value is not of the type the RFC gives it is `None` too, with its JSON kept in the record's `extra`; and properties the crate has no field for are kept there. No JSON object fails to decode as a record, so one odd value from one server costs one property of one record, not the whole answer.
- **Limits.** Each API exchange has a timeout (30 seconds by default) and a largest response size (64 MiB by default; 4 MiB for the session), counted after decompression. `IdBatches` asks for fewer records per call when an answer is over the limit.
- **Compression.** With the `gzip` feature the client sends `Accept-Encoding: gzip` and decompresses answers itself, over any transport, never past the response size limit.
- **Result references** carry the data type whose ids they resolve to (`query.ids()`, `changes.created()`, `emails.thread_ids()`), so feeding a call the ids of another type does not compile; and a request whose reference names no earlier call of the same request is refused before it is sent.
- **Nothing is sent twice** by the client on its own, except once after a 401 when the credentials are a token source that can refresh.
- **Dates** stay the RFC 3339 text the server sent (`mail::UtcDate`); the crate depends on no date library.
- **Unknown data** is kept, not dropped: capability objects, extra properties on records and errors, and error types from later specifications.
- **Removing with a patch** is `PatchObject::remove`, which writes `null` as RFC 8620 section 5.3 says (not `false`). `mail::EmailUpdate`, the typed patch for an email's keywords and mailboxes, has no way to write anything else.
- **Blobs are whole bodies.** An upload is given as bytes and a download returns bytes, each under limits apart from the API's: a download size limit counted after decompression (128 MiB by default, or the download's own), the session's `maxSizeUpload` checked before an upload is sent, and a blob timeout (five minutes by default). There is no streaming yet; `Client::download_url` gives the URL to a caller who must stream a blob to disk with an HTTP client of their own.
- **Credentials and blobs.** The upload and download URLs are the session's and get the credentials, whatever host they are on. A redirect from them is judged by the `RedirectPolicy` like any other; a download can opt into following a redirect the policy does not allow without credentials (`BlobDownload::follow_redirects_without_credentials`), for a server that answers with a signed link to a storage host.

## Features

- `reqwest` (default): `ReqwestTransport` and `ClientBuilder::connect`. Without it the crate needs no async runtime: implement `HttpTransport` for your own HTTP client.
- `gzip` (default): gzip-compressed answers, decompressed with `flate2`.

Without either, the crate depends only on `serde`, `serde_json`, `url`, `base64` and `thiserror`.

## Minimum Rust version

The workspace's: Rust 1.98.

## Licence

GPL-3.0-only for now, as the rest of the Zmail workspace. The crate is to become a project of its own, and its licence is to change when it does (MIT OR Apache-2.0 is what is expected of a library; the choice is made at that point). So that this stays possible, it is written only from the RFCs and this project's own code: nothing in it is copied from another JMAP crate, and contributions must keep to that.
