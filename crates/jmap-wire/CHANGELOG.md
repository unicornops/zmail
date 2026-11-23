# Changelog

Changes to `jmap-wire` that its users would notice. The crate is not published yet; versions here are the crate's own, not Zmail's.

## Unreleased

- The protocol core: `Request`, `Response`, `Invocation`, result references, `createdIds`, and `using` worked out from the calls in a request.
- `Session`, with URL template expansion for the upload, download and event source endpoints, and `discovery::session_url`.
- Errors: `ProblemDetails`, `MethodError`, `SetError`, each keeping types and properties the crate does not know.
- `Get`, `Set` and `PatchObject` for any data type; `Identity`, `EmailSubmission` and `Email` (set only) in `mail`.
- `Client` over `HttpTransport`, with `Credentials` (Basic, Bearer, a refreshing `TokenSource`), `RedirectPolicy`, a timeout and a response size limit; `ReqwestTransport` behind the `reqwest` feature.
