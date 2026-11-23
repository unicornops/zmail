# Changelog

Changes to `jmap-wire` that its users would notice. The crate is not published yet; versions here are the crate's own, not Zmail's.

## Unreleased

- The protocol core: `Request`, `Response`, `Invocation`, result references, `createdIds`, and `using` worked out from the calls in a request.
- `Session`, with URL template expansion for the upload, download and event source endpoints, and `discovery::session_url`.
- Errors: `ProblemDetails`, `MethodError`, `SetError`, each keeping types and properties the crate does not know.
- `Get`, `Set` and `PatchObject` for any data type; `Identity`, `EmailSubmission` and `Email` (set only) in `mail`.
- `Client` over `HttpTransport`, with `Credentials` (Basic, Bearer, a refreshing `TokenSource`), `RedirectPolicy`, a timeout and a response size limit; `ReqwestTransport` behind the `reqwest` feature.
- Reading mail: `Mailbox`, `Thread` and `Email` in `mail` with `/get`, `Changes`, `Query` and `QueryChanges` for any data type, `Filter` with `AND`, `OR` and `NOT`, `Comparator`, the Email and Mailbox filter conditions, header fields in every parsed form (`HeaderProperty`, `EmailRecord::header`), `Email/get`'s body arguments, body parts and body values.
- Records are lenient: every property optional, values of another type than the RFC's read as `None` and kept in `extra`.
- `IdsReference`: result references that say which data type's ids they resolve to (`Call::ids`, `created`, `updated`, `thread_ids`, `email_ids`, `ids_in`); `Get::properties_from`.
- `IdBatches` and `Client::get_in_batches`; `Error::is_request_too_large`, `is_response_too_large`, `as_method_error`.
- The `gzip` feature (on by default): `Accept-Encoding: gzip`, with the response size limit counted on the decompressed body.
- `Response::take`, `HttpRequest::new`, `Date`; `GetResponse` can be written back as JSON.
- Writing mail: `mail::NewEmail` with a setter per property an email may be created with, `mail::NewBodyPart`, and `mail::EmailUpdate`, a typed patch for keywords and mailboxes that clears and removes with `null`.
- `mail::EmailImport` (`Email/import`), `CopyMethod` (`/copy` for any data type, with `mail::EmailCopy`), and `Mailbox/set` (`mail::NewMailbox`, `mail::MailboxUpdate`, `mail::MailboxSetArguments`).
- `SetOutcome` and `SetResponse::outcome_of_create`, `outcome_of_update` and `outcome_of_destroy`; `SetError::names_property`, `existing_id` and `blobs_not_found`.
- Blobs: `Client::upload`, `Client::download` and `Client::download_url`, with `BlobDownload`, `Blob` and `UploadedBlob`; `ClientBuilder::blob_timeout` and `max_download_bytes`; `Error::UploadTooLarge` and `Error::http_status`.
- `Date` converts from text.

Changes to what was there before:

- `Set::update` takes `impl Into<PatchObject>`, so a typed patch is passed as it is. A call that wrote `.into()` on its patch no longer compiles (the conversion is ambiguous): take the `.into()` away.
- The `created` records of `Set<Email>` are `mail::EmailRecord`s, where they were maps of JSON values.

- `Get::ids_from` takes `impl Into<IdsReference<O>>`. A `ResultReference` still converts, so calls that pass one compile as before.
- A request with a result reference that names no earlier call of the same request, a call of another method, or an argument that is also given outright is refused with `Error::InvalidResultReference`: by `Request::call` for the call being added, and by `Client::send` for the whole request. Before, it was sent and the server answered `invalidResultReference`.
- Requests carry `Accept-Encoding: gzip` unless the `gzip` feature is turned off.
- An `EmailAddress` whose `email` is missing or `null` reads as an empty address instead of failing to decode.
- `Client::call` moves the answer out of the response instead of copying it.
