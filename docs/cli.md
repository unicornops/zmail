# The command line

`zmail-app` with no command opens the desktop app. With a command, it works on the same store and accounts as the desktop app and the [terminal UI](./tui.md) (`zmail-app tui`), and needs the same system keychain (see [Installing Zmail](./install.md#first-run)). The desktop app and the terminal UI add accounts themselves; the commands are for scripts, quick checks and machines without a window.

From a source checkout, put `cargo run -p zmail-app --` in front of a command: `cargo run -p zmail-app -- list-accounts`. With no command, `cargo run -p zmail-app` launches the desktop client.

## Help and usage errors

`zmail-app --help` (or `-h`) lists every command; `--help` after a command shows that command's arguments and options instead of running it, so `zmail-app rotate-key --help` never rotates anything. `zmail-app help move` does the same. `zmail-app --version` (or `-V`) prints the version.

Zmail never runs a command it does not fully understand. An unknown command or option, a missing argument or one too many is a usage error: Zmail prints what was wrong and the command's usage, runs nothing and exits with status 2. Any other failure exits with status 1. `--` ends the options, so every word after it is an argument (a search word that starts with `--`, for example).

```console
$ zmail-app move 42
error: move needs MAILBOX
Usage: zmail-app move THREAD MAILBOX
Run `zmail-app move --help` for more.
$ echo $?
2
```

The help of Zmail 0.34.1, `zmail-app --help`:

```text
Zmail 0.34.1: email on the desktop and in the terminal.

Usage: zmail-app [COMMAND] [ARGUMENTS] [OPTIONS]
With no command, zmail-app opens the desktop app.

Reading and triage:
  tui                         Read and write mail in the terminal
  sync [all|ACCOUNT]          Sync every account, or one
  mailboxes ACCOUNT           List an account's mailboxes and their ids
  search ACCOUNT QUERY...     Search an account's mail
  index ACCOUNT               Fetch and index message bodies for search
  read THREAD                 Print a conversation
  attachments THREAD          List a conversation's attachments
  save-attachment ATTACHMENT [DIRECTORY]
                              Save an attachment into a folder
  flag THREAD                 Flag a conversation
  unflag THREAD               Remove a conversation's flag
  mark-read THREAD            Mark a conversation read
  mark-unread THREAD          Mark a conversation unread
  move THREAD MAILBOX         Move a conversation to another mailbox
  archive THREAD              Move a conversation to the Archive
  delete THREAD               Move a conversation to the Trash

Writing and sending:
  compose ACCOUNT [IDENTITY]  Write a draft at prompts, then send it or keep it
  drafts [ACCOUNT]            List drafts
  draft DRAFT                 Print a draft
  attach DRAFT PATH           Attach a file to a draft
  detach ATTACHMENT           Remove an attachment from a draft
  send DRAFT                  Send a draft
  outbox [ACCOUNT]            List the outbox's jobs
  retry-outbox JOB            Try a failed outbox job again
  process-outbox [ACCOUNT]    Send what is due in the outbox now
  mailto URL                  Open the window on a draft from a mailto: link

Accounts:
  list-accounts               List accounts
  add-imap                    Add an IMAP account at prompts, and sync it
  add-jmap                    Add a JMAP account at prompts, and sync it
  cache-budget ACCOUNT [MIB|unlimited]
                              Set or remove an account's disk budget for mail
  empty-trash ACCOUNT         Delete everything in an account's Trash for good
  dead-ops [ACCOUNT]          List changes the server kept refusing
  retry-op OPERATION          Queue a refused change again
  discard-op OPERATION        Drop a refused change
  export-account ACCOUNT PATH
                              Write an account's stored mail to an mbox file

The mail store:
  backup [DIRECTORY]          Back up the mail store, keeping the 10 newest
  rotate-key                  Re-encrypt the store (quit Zmail first)
  reset-store                 Start over when the store's key is lost

Fixtures and development:
  launch-ui-fixture VIEW [dark|light|system] [TEXT_SIZE]
                              Open the window on synthetic mail in one UI state
  launch-tui-fixture [VIEW]   Open the terminal UI on synthetic mail
  launch-imap-fixture ENDPOINT USERNAME EMAIL
                              Open the window on a test IMAP server, in memory

Options:
  -h, --help                  Show this help, or after a command, its help
  -V, --version               Show the version

Only one Zmail process syncs a store at a time. While the desktop app or
`zmail-app tui` syncs it, sync, index and process-outbox stop with an error,
and send, retry-outbox, compose, add-imap, add-jmap and empty-trash do their
part and leave the syncing and sending to that Zmail. --force runs them
here anyway, beside it.
```

## Accounts

Add and sync an account from the command line:

```bash
zmail-app add-imap
zmail-app add-jmap
```

Both ask for the account's details at prompts and check each answer as it is given. Before anything is stored, Zmail tests the sign-in on the incoming server (IMAP, or the JMAP session) and on the outgoing one (SMTP), as the desktop app and the terminal UI do; if either fails it says why and adds nothing. Then it stores the account, with its password or token in the system keychain, and runs the first sync.

A server without TLS (`imap://`, `http://` for JMAP, or SMTP security `plain`) is refused unless it is on this machine (loopback) or `--allow-cleartext` is given:

```bash
zmail-app add-imap --allow-cleartext     # e.g. a test server on your network
```

Inspect configured accounts or trigger sync:

```bash
zmail-app list-accounts
zmail-app sync all
zmail-app sync 1
```

`cache-budget`, `dead-ops`, `retry-op`, `discard-op` and `export-account` are listed in the help above; `zmail-app COMMAND --help` says more about each.

## One Zmail syncs at a time

Only one Zmail process syncs a store at a time (see [Terminal UI](./tui.md#one-zmail-syncs-a-store)). While the desktop app or `zmail-app tui` syncs it, `sync`, `index` and `process-outbox` say so and exit with an error, and `send`, `retry-outbox`, `compose`, `add-imap`, `add-jmap` and `empty-trash` do their part (queue the message, store the account, empty the Trash on the server) and leave the syncing and sending to that Zmail, which does it within seconds. `--force` runs them here anyway, beside it. Commands that only read the store, such as `list-accounts`, `search` and `read`, work regardless; `search --server` asks the server too, and leaves fetching what it finds to the Zmail that syncs.

## Reading and triage

Search or operate on locally mirrored mail:

```bash
zmail-app search 1 incident
zmail-app read 42
zmail-app flag 42
zmail-app mark-read 42
zmail-app mailboxes 1
zmail-app move 42 7
zmail-app move 42 archive
zmail-app move 42 "Old projects"
zmail-app archive 42
zmail-app delete 42
```

- `mailboxes ACCOUNT` lists an account's mailboxes with their ids, roles and conversation counts. `move` takes a mailbox of the conversation's account by its id, its role (`inbox`, `drafts`, `sent`, `archive`, `junk` or `trash`) or its name (exactly, or else ignoring case), as `mailboxes` lists them.
- `delete` moves a conversation to the Trash. A conversation already in the Trash, or in an account with no Trash, is deleted for good instead: that asks first in a terminal, and elsewhere (a script, a pipe) it needs `--yes`.
- `empty-trash ACCOUNT` expunges everything in the account's Trash on the server, which can't be undone. It asks first too, or takes `--yes`:

  ```bash
  zmail-app empty-trash 1 --yes
  ```

- `attachments THREAD` lists a conversation's attachments and `save-attachment ATTACHMENT [DIRECTORY]` saves one into a folder (the current one by default). It never replaces a file of the same name unless `--overwrite` is given:

  ```bash
  zmail-app save-attachment 12 ~/Downloads --overwrite
  ```

Searches use the same language as the app (`from:`, `has:attachment`, `after:30d` and so on); see [Searching mail](./search.md):

```bash
zmail-app search 1 'from:alice "budget review" -in:junk after:30d'
zmail-app search --server 1 invoice      # also ask the server about mail not indexed yet
zmail-app index 1                        # one indexing pass, in the foreground
```

## Writing and sending

`compose ACCOUNT [IDENTITY]` writes a draft at prompts and then sends it or keeps it. `drafts`, `draft`, `attach`, `detach` and `send` work on drafts by id, and `outbox`, `retry-outbox` and `process-outbox` on the outbox. `mailto URL` opens the desktop app on a new draft from a `mailto:` link; the system's mail link handler runs `zmail-app mailto:...` with the whole link.

## The mail store

`backup [DIRECTORY]` backs up the encrypted store (to `backups` beside the store by default, keeping the 10 newest; backups are encrypted with the store's key). `rotate-key` re-encrypts the store with a new key (quit Zmail first; backups made with the old key no longer open). `reset-store` starts over when the store's key is lost: it moves the unreadable store aside, and refuses while the store still unlocks. See [ADR 0005](./adr/0005-store-encryption.md) and [Where Zmail keeps its data](./install.md#where-zmail-keeps-its-data).

## Fixtures and development

These open Zmail on synthetic or test mail, with nothing saved; see [Development](./development.md).

- `launch-ui-fixture VIEW [dark|light|system] [TEXT_SIZE]` opens the window on one UI state, for the [screenshot matrix](./development.md#ui-screenshots).
- `launch-tui-fixture [VIEW]` runs the terminal UI on the same synthetic mail, with a keychain in memory.
- `launch-imap-fixture ENDPOINT USERNAME EMAIL` opens the window on a test IMAP server, in memory. The password is read from the `ZMAIL_FIXTURE_SECRET` environment variable, or else from standard input, never from the command line:

  ```bash
  ZMAIL_FIXTURE_SECRET=phase2-password zmail-app launch-imap-fixture imap://127.0.0.1:3143 phase2 phase2@localhost
  ```
