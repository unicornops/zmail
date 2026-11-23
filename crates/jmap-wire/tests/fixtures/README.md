# Response fixtures

One directory per server, one file per response body, exactly as the
server sent it. `tests/fixtures.rs` decodes every file in every directory
by the first part of its name, so a new server is a new directory:

| File name starts with | Decoded as |
| --- | --- |
| `session` | `Session` |
| `problem` | `ProblemDetails` |
| `identity-get` | `Response`, then `GetResponse<IdentityRecord>` |
| `identity-set` | `Response`, then `SetResponse<IdentityRecord>` |
| `emailsubmission-set` | `Response`, then `SetResponse<EmailSubmissionRecord>` |
| `email-set` | `Response`, then `SetResponse<..>` of `Email` |
| `method-error` | `Response`, then `MethodError` |

The part of the name after the first `.` says which case it is and is
not interpreted.

## Where they come from

- `rfc`: the examples of RFC 8620 (sections 2.1, 3.6.1.1 and 3.6.2) and
  RFC 8621 (sections 6.4 and 7.5.1), wrapped in a Response object where
  the RFC shows only the method responses. The elided parts (`...`) are
  left out, and the two slips in the session example are put right (a
  missing comma, `maxConcurrentRequest`).
- `james-3.9`: written by hand in the shapes Apache James 3.9 answers
  with (`null` for `state` and `notFound`, a `sortOrder` on identities,
  its error descriptions), with synthetic ids and addresses. They are
  not captures; replace them with captures when someone records some.

No fixture may contain real addresses, tokens or message content.
