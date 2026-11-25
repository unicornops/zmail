# Searching mail

Zmail searches the local copy of your mail first, in the desktop app, the [terminal UI](./tui.md) (`W`, WhereIs) and the [`search` command](./cli.md#reading-and-triage).

## The search language

Search queries (in the app's search field and the `search` command) combine
words, `"quoted phrases"` and operators, all of which must match:
`from:`, `to:`, `subject:`, `has:attachment`, `is:unread`, `is:read`,
`is:flagged`, `in:<mailbox name or role>`, and `before:`/`after:` with a
`YYYY-MM-DD` date or a relative one such as `7d`, `2w`, `3m` or `1y`. A
leading `-` negates any of them:

```bash
zmail-app search 1 'from:alice "budget review" -in:junk after:30d'
```

`in:` takes a mailbox's role (`in:inbox`, `in:sent`, `in:spam` or
`in:junk`, `in:bin` or `in:trash`), its full name, or the name the folder
list shows for it. A folder inside another one can be named either way:
`in:Invoices` searches every folder called Invoices, such as both
`Clients/Invoices` and `Suppliers/Invoices`, while `in:Clients/Invoices`
searches just that one. The full name uses the account's own separator,
so an IMAP server that nests with dots wants `in:INBOX.Invoices`. A
Proton folder is found by its name too (`in:Work` for Bridge's
`Folders/Work`). Names ignore case, and `-in:Invoices` leaves out every
folder called Invoices.

## Where a search looks

In the app, a search starts in the folder the list shows (the Unified
Inbox counts as one folder: every account's inbox). The scope control under
the list header, the empty state and the Edit menu widen it to the folder's
account or to every account (Secondary+Alt+1, 2 and 3 pick folder, account
and all accounts). Each new search starts in the folder again. An `in:`
operator in the query names the folder itself and takes the folder's place
in the scope control. The `search` command searches one account.

## Indexing and server search

The desktop app indexes bodies in the background; `zmail-app index ID` runs
one indexing pass for an account in the foreground and prints its progress.
IMAP message bodies are fetched and indexed newest first, within the
account's disk budget, and pause on battery power (which Zmail can tell
only on Linux so far) unless Settings → Mail says to keep going; Settings → Mail also turns background indexing off (see
[Features](./features.md#settings--mail)).
Until every body is indexed, a search with free text also asks the server
(IMAP `UID SEARCH`, JMAP `Email/query`); its extra hits are listed after the
local ones and marked "found on server". The server search follows the
scope: in folder scope IMAP searches only that folder's mailboxes, and a
search across accounts asks each account's server separately, so one
offline account does not stop the others' results. `zmail-app search --server ID QUERY`
does the same from the command line.
