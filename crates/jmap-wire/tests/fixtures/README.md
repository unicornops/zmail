# Response fixtures

One directory per server, one file per response body, exactly as the
server sent it. `tests/fixtures.rs` decodes every `.json` file in every
directory, so a new server is a new directory:

| File name starts with | Decoded as |
| --- | --- |
| `session` | `Session` |
| `problem` | `ProblemDetails` |
| anything else | `Response`; then each method response in it by its name |

A response file's first method response must be what the file's name
says: `mailbox-get`, `mailbox-changes`, `mailbox-query`, `thread-get`,
`email-get`, `email-query`, `email-changes`, `email-querychanges`,
`email-set`, `identity-get`, `identity-set`, `emailsubmission-set`,
`method-error` (an `error` response), or `chained` (several calls, any
first). The part of the name after the first `.` says which case it is
and is not interpreted. Records are also written back as JSON and
compared with what was sent, so a fixture shows that nothing the server
sent is lost.

Two names have a second use. Zmail's own tests
(`crates/zmail-jmap/src/read_characterisation_tests.rs`) replay any
directory that has both `mailbox-get.all.json` (every mailbox of an
account) and `email-get.sync.json` (the answer to an `Email/get` of the
properties Zmail's sync asks for, with `fetchAllBodyValues`) through a
whole sync, and pin what is stored. With an `email-get.light.json` (the
same emails with only `id`, `threadId`, `mailboxIds`, `keywords` and
`hasAttachment`, as after a change of keywords or mailboxes) the
incremental sync is replayed too.

## Where they come from

- `rfc`: the examples of RFC 8620 (sections 2.1, 3.6.1.1 and 3.6.2) and
  RFC 8621 (sections 2.6, 3.1.1, 4.2.1, 4.10, 6.4 and 7.5.1), wrapped in
  a Response object where the RFC shows only the method responses. The
  elided parts (`...`) are left out, and the two slips in the session
  example are put right (a missing comma, `maxConcurrentRequest`).
- `james-3.9`: see the README in that directory.
- `shapes`: written by hand, not captured from any server. They are the
  shapes the RFCs allow and servers differ on: properties left out,
  `null` or empty; keywords and mailbox ids mapped to `false`; parts
  without a type, a name or a blob; dates with an offset or fractions of
  a second; unknown roles. `email-get.lenient.json` goes further, with
  values of other types than the RFC's and properties from no RFC, which
  must cost only the property concerned.

## Adding a server

Record the response bodies of the calls above against a test account
with synthetic mail, and put them in a directory named after the server
and its version. The requests Apache James was sent are in
`james_answers_the_read_calls_for_the_jmap_wire_fixtures`
(`crates/zmail-test-support/tests/live_jmap_harness.rs`); the same
messages and calls against another server give fixtures that can be
compared.

No fixture may contain real addresses, tokens or message content.
