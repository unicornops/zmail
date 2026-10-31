# ADR 0007: A Terminal UI as a Second Frontend

## Status

Proposed on October 31, 2026. The questions for Rob are under
"Decisions needed" at the end.

## Context

Rob asked whether Zmail can gain a terminal UI (TUI) as an extra option,
without major changes to what exists. The workspace is already split so
that only the GUI depends on GPUI:

| Crate | Depends on GPUI |
| --- | --- |
| `zmail-core`, `zmail-store`, `zmail-security`, `zmail-notify`, `zmail-auth` | No |
| `zmail-sync`, `zmail-imap`, `zmail-jmap`, `zmail-smtp`, `zmail-proton` | No |
| `zmail-render` (the renderer boundary, ADR 0006) | No |
| `zmail-ui`, `zmail-render-web`, `zmail-app` (which links `zmail-ui`) | Yes |

What already works without the GUI:

- **A command-line frontend.** `zmail-app` has about 40 subcommands on
  `LocalStore` and `SyncService`, including `list-accounts`, `sync`,
  `search`, `read`, `flag`, `move`, `archive`, `delete`, `compose`,
  `send`, `outbox`, `attachments`, `save-attachment`, `backup`,
  `rotate-key`, `add-imap` and `add-jmap`. They unlock the encrypted store
  through the OS keychain (ADR 0005) exactly as the GUI does.
- **Local-first reads.** The GUI reads only the store. Background sync
  writes to it, and the workspace re-reads it every 5 seconds
  (`WorkspaceView::refresh_from_store`). A TUI can do the same with no
  store change.
- **Text rendering.** `zmail_render::prepare` gives every message a text
  form and the link side table (`zmail-link:<n>` → `LinkTarget`). That
  is what the GUI's Simple view shows.
- **Safe concurrent use.** Every open store holds a shared advisory lock,
  so a GUI and a TUI can have the same store open at once. Outbox jobs are
  claimed atomically (`claim_due_outbox_job`), so two processes cannot
  send the same message twice.

What lives only in the GUI crate today:

- Account setup: the form, server discovery, the Google sign-in guide
  and Proton sign-in (`zmail-ui/src/account_setup*`, `google_oauth.rs`).
  About 3,600 lines, mostly view code.
- Undoable mail actions (`mail_actions.rs`): archive, move, delete, flag
  and read with an undo stack. It uses GPUI only for `Context` and
  `EventEmitter`.
- Key bindings, search parsing helpers and the command palette, all tied
  to GPUI actions.

A terminal adds one risk the GUI does not have: **escape-sequence
injection**. Mail text is attacker-controlled. Printed to a terminal
unfiltered, an ANSI or OSC sequence in a subject or body can rewrite the
screen, fake prompts, set the window title, write the clipboard
(OSC 52), or plant hyperlinks (OSC 8). The renderer's text output is safe
for GPUI, which draws characters, but has never been filtered for a
terminal.

## Options considered

| Option | Notes | Verdict |
| --- | --- | --- |
| **ratatui + crossterm** | The standard Rust TUI stack, maintained. crossterm covers Linux, macOS and Windows terminals. Immediate-mode drawing, with `TestBackend` for snapshot tests. | **Recommended** |
| cursive | Retained-mode widgets, more batteries included. Smaller community, and its default backend is ncurses (C) on Unix. | No |
| Extend the CLI only | No new dependencies, but no interactive browsing, so it doesn't meet the request. | No |
| A separate binary | Keeps the GUI out of the TUI build, but means a second artefact in every installer and two places to unlock and configure the store. | Not now; a feature flag gives the same benefit |

## Decision (proposed)

### 1. Shape

- A new crate, **`zmail-tui`**, depends on `zmail-core`, `zmail-store`,
  `zmail-sync`, `zmail-render`, `zmail-security` and `zmail-notify`. It
  never depends on GPUI, `zmail-ui` or `zmail-render-web`.
- It is started with **`zmail-app tui`**, behind a `tui` Cargo feature of
  `zmail-app`. Store path, keychain, store lock and preferences are shared
  with the GUI and the CLI.
- **`unsafe_code = "forbid"`**, like every crate except the renderer's
  platform modules.

### 2. Layout and interaction

- Three panes as in the GUI: accounts and mailboxes, the conversation
  list, and the reader. Below about 100 columns they collapse to one pane
  at a time.
- Keys follow common terminal mail clients and Zmail's Gmail keymap:
  `j`/`k`, `Enter`, `e` archive, `#` delete, `s` flag, `u` unread, `/`
  search, `c` compose, `r`/`a`/`f` reply, reply-all and forward, `z` undo,
  and `?` for help. The keymap is data, so it can be remapped later.
- Search uses the store's search (the same syntax as the GUI), including
  the server search fallback.
- The status line shows sync progress and failures from the store's sync
  records, as the sidebar tooltip does.

### 3. Reading mail

- The reader shows `PreparedMessage`'s text form: the Simple view. The
  formatted HTML view, remote images and inline images don't exist in the
  TUI.
- Links show as numbered references (`[3]`). Opening one shows the real
  URL and asks before handing it to the OS opener, as the GUI's link
  dialog does.
- Attachments are listed and saved through `AttachmentBlob::save_to`,
  with the sanitised file name (`attachment_safety`).

### 4. Terminal safety (required before any mail text is shown)

- A single function, **`zmail_security::terminal::sanitize`**, filters
  every string from mail before it reaches the terminal: subjects,
  senders, bodies, file names, link text and URLs, mailbox names and
  server error messages.
- It removes:
  - C0 controls except newline and tab;
  - DEL and all C1 controls (U+0080–U+009F);
  - ESC and anything it introduces (CSI, OSC, DCS, APC, PM);
  - the bidirectional overrides and zero-width characters that
    `attachment_safety` already strips.
- Tabs become spaces.
- The TUI itself emits no OSC 8 hyperlinks and no OSC 52 clipboard
  writes. Copy goes through the OS clipboard.
- A hostile corpus test sits next to the HTML one. It covers CSI cursor
  moves, OSC 0/2 titles, OSC 52, OSC 8, DCS, 8-bit C1 introducers and
  bidi tricks, and asserts that no byte below 0x20 (except `\n`) and no
  ESC or C1 survives.

### 5. Writing mail

- The compose pane has a built-in editor (`tui-textarea`), with
  To/Cc/Bcc/Subject fields and the draft saved to the encrypted store as
  the GUI does. Send uses `SyncService::queue_draft_and_send(_after)`,
  so undo-send works the same way.
- `$EDITOR` is **off by default**. Handing the draft to an external
  editor means writing it as a plaintext temporary file, which ADR 0005
  otherwise rules out. If enabled, the file goes in a 0700 directory and
  is overwritten and deleted after the editor exits, and the setting
  says so.

### 6. Sync and notifications

- Only one process runs sync. A second advisory lock beside the store
  (`<database>.sync.lock`, held exclusively) is taken by whichever
  frontend starts first.
  - If the GUI holds it, the TUI shows the store, refreshes it every
    5 seconds and queues its actions in the store. The GUI's sync service
    sends them.
  - If the TUI holds it, it runs `SyncService` itself.
  - The GUI gains the same check. This is the one change outside the new
    crate that's needed for correctness, and it avoids duplicate IDLE and
    push connections and double polling.
- New mail shows in the status line and rings the terminal bell
  (configurable). No OS notifications from the TUI in v1.

### 7. What moves out of `zmail-ui`

Only what the TUI actually needs, each in its own small PR:

1. `mail_actions.rs` moves to a new GPUI-free crate,
   `zmail-actions`, for undoable archive, move, delete, flag and read.
   Change events go through a plain callback instead of GPUI's
   `EventEmitter`, and `zmail-ui` wraps it.
2. Later, account setup's non-visual parts (validation, discovery, the
   OAuth loopback flow) move to the same crate, so the TUI can add
   accounts.

## Plan

Each phase ships on its own, in normal releases (alpha; no previews).

| Phase | Work | Gate |
| --- | --- | --- |
| 0. Foundations | `zmail_security::terminal::sanitize` and its hostile corpus; the sync lock in `zmail-store`, used by the GUI | Corpus tests; GUI behaviour unchanged |
| 1. Read | `zmail-tui`: unlock, accounts and mailboxes, conversation list, reader, search, status line, refresh; `zmail-app tui` behind the `tui` feature | `TestBackend` snapshot tests for each view; manual check in a real terminal on each OS |
| 2. Triage | Extract `zmail-actions`; archive, move, delete, flag and read with undo | Shared tests pass for GUI and TUI |
| 3. Write | Compose, reply, reply-all and forward, drafts, attachments, send with undo-send, the outbox | A draft round-trips through the store; live harness send test |
| 4. Accounts | Add IMAP, JMAP and SMTP accounts in the TUI; OAuth (Google, Microsoft) through the browser and loopback; Proton sign-in, opening the browser for human verification | As phase 3 |

Until phase 4, accounts are added with the GUI or with `zmail-app
add-imap` and `add-jmap`; with no account, the TUI says so.

## Risks

- **Headless machines.** The store key lives in the OS keychain (ADR 0005).
  Over SSH to a Linux machine with no Secret Service running, the TUI
  can't unlock the store, and there is no plaintext fallback. A
  passphrase-protected key would solve this, but that's a separate ADR.
- **Terminal differences.** Colours, Unicode width and key reporting vary
  (Windows Terminal, the macOS Terminal and iTerm2, Linux consoles).
  crossterm hides most of this. Wide characters and emoji in subjects
  need width-aware truncation (`unicode-width`, already in the tree).
- **Two frontends to keep in step.** New store features need TUI work
  too. Keeping the shared logic in `zmail-actions` limits that.
- **Accessibility.** Terminals depend on the screen reader's terminal
  support. The TUI keeps a linear, plain layout available.

## Decisions needed

1. **Go ahead with the TUI as proposed**: a new `zmail-tui` crate, run as
   `zmail-app tui`.
2. **New dependencies**: `ratatui`, `crossterm` and `tui-textarea`.
3. **The `tui` feature on by default** in release builds, so every
   download has it. Recommended: yes, since it adds well under a
   megabyte.
4. **`$EDITOR` support** off by default and opt-in, with the plaintext
   temporary file explained.
5. **The sync lock**: one process syncs. This is the one change to the
   GUI's behaviour.
