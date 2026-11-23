# Apache James 3.9

Most of these are **captures**: what the Apache James 3.9 of Zmail's mail
harness (`docker/mail-harness`) answered in CI on 2026-11-23, taken from
the log of the live test
`james_answers_the_read_calls_for_the_jmap_wire_fixtures`
(`crates/zmail-test-support/tests/live_jmap_harness.rs`), which delivers
three probe messages and prints the answers. They are reformatted
(indentation, object members in alphabetical order) and otherwise as
James sent them. The account, its addresses and the messages are the
harness's own; nothing in them is real mail.

| File | The call |
| --- | --- |
| `session.json` | the Session object |
| `mailbox-get.all.json`, `mailbox-get.ids-null.json` | `Mailbox/get` without `ids`, and with `"ids": null` |
| `mailbox-get.some-properties.json` | `Mailbox/get` of `name`, `role` and `parentId` |
| `email-get.sync.json` | `Email/get` of the three probes with the properties Zmail's sync asks for and `fetchAllBodyValues` |
| `email-get.light.json` | the same emails' `threadId`, `mailboxIds`, `keywords` and `hasAttachment` |
| `email-get.default-properties.json` | `Email/get` without `properties` |
| `email-get.structure.json` | `blobId`, `size`, `sentAt`, `sender`, `bcc`, `headers` and `bodyStructure` of the multipart probe, with `bodyProperties` (among them `subParts`, `headers` and `header:Content-Type`), the text and HTML body values and `maxBodyValueBytes` of 16 |
| `email-get.header-forms.json` | header fields of the multipart and the bare probe in every parsed form, single and `:all`, present and absent |
| `email-get.not-found.json` | one email that exists and one id that does not |
| `email-get.state.json` | `Email/get` of no ids, for the state |
| `email-query.page.json` | `Email/query`, newest first, the first two |
| `email-query.filtered.json` | `Email/query` with an `AND` of a text condition and a `NOT`, and `collapseThreads` |
| `email-changes.none.json` | `Email/changes` since the current state |
| `thread-get.json` | `Thread/get` of the probes' threads |
| `chained.query-get.json` | an `Email/query` and an `Email/get` of its `/ids` in one request |
| `method-error.email-changes-cannot-calculate.json`, `method-error.mailbox-changes-since-unknown.json` | `/changes` since a state James never gave (it answers `invalidArguments`, not `cannotCalculateChanges`) |
| `method-error.email-get-unknown-property.json` | `Email/get` of a property that does not exist |
| `method-error.email-querychanges.json` | `Email/queryChanges`, which James does not have |
| `method-error.mailbox-query-sorted.json` | `Mailbox/query` with `hasAnyRole`, which James does not filter on |

What they show of James that the RFCs do not say, and that a decoder has
to take:

- a property that has no value is left out more often than it is `null`
  (an address without a name has no `name`; a part has no `name`, `cid`,
  `disposition`, `language` or `location` unless the message gives one),
  but a header field that is absent is `null`, or `[]` with `:all`;
- `multipart/*` parts have a `partId` and a `charset`, and no `blobId`;
- an email's `keywords` is `{}` when it has none;
- an `Email/query` answer has a `limit` (256) whenever the call gave none.

Four files are **written by hand** in James's shapes, from step 1 of ADR
0008, and are not captures: `identity-get.json`,
`emailsubmission-set.invalid-arguments.json`,
`emailsubmission-set.forbidden-from.json` and
`email-set.destroyed.json`.
