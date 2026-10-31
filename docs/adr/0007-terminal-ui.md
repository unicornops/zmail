# ADR 0007: A Terminal UI as a Second Frontend

## Status

Accepted on October 31, 2026, with Rob's decisions recorded under
"Decisions" at the end. Proposed the same day.

Progress:

- Phase 0: `zmail_security::terminal::sanitize` (and `sanitize_line` for
  one-line fields) with its hostile corpus, `tests/terminal_corpus.rs`.

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
| **ratatui + crossterm** | The standard Rust TUI stack, maintained: ratatui 0.30.2 (June 2026), crossterm 0.29 (its default backend, commits in October 2026). crossterm covers Linux, macOS and Windows terminals. Immediate-mode drawing, with `TestBackend` for snapshot tests. | **Chosen** |
| cursive | Retained-mode widgets, more batteries included. Smaller community, and its default backend is ncurses (C) on Unix. | No |
| Extend the CLI only | No new dependencies, but no interactive browsing, so it doesn't meet the request. | No |
| A separate binary | Keeps the GUI out of the TUI build, but means a second artefact in every installer and two places to unlock and configure the store. | Not now; a feature flag gives the same benefit |

## Decision

### 1. Shape

- A new crate, **`zmail-tui`**, depends on `zmail-core`, `zmail-store`,
  `zmail-sync`, `zmail-render`, `zmail-security` and `zmail-notify`. It
  never depends on GPUI, `zmail-ui` or `zmail-render-web`.
- It is started with **`zmail-app tui`**, behind a `tui` Cargo feature of
  `zmail-app` that is **on by default**, so every release has it. Store path, keychain, store lock and preferences are shared
  with the GUI and the CLI.
- **`unsafe_code = "forbid"`**, like every crate except the renderer's
  platform modules.

### 2. Layout and interaction: Pine/Alpine, with modern touches

Rob chose the Pine/Alpine model over mutt's: a menu-driven app whose
commands are always on screen, a hierarchy of full-screen views, and
single-letter commands, with modern touches on top.

**The Alpine structure, kept:**

- **Three fixed bands on every screen:**
  - a title bar: app name, the screen's name, the current folder and the
    position ("Msg 3 of 120");
  - the screen's body;
  - a status line, then a two-line **command bar** listing the keys that
    work on this screen (`? Help  < FldrList  P PrevMsg  N NextMsg  D Delete
    R Reply  F Forward  S Save  W WhereIs  O OTHER CMDS`). `O` pages through
    more commands, as in Alpine. Nobody has to remember a key.
- **Screens as a hierarchy:**
  - Main Menu (`C` Compose, `I` Message Index, `L` Folder List,
    `A` Address Book, `S` Setup, `Q` Quit);
  - Folder List, where accounts are collections, as Alpine shows them;
  - Message Index;
  - Message Text;
  - Compose.
  - `<` goes up a level, `>` and Enter go down, and `M` always returns to
    the Main Menu.
- **Alpine's mnemonics where Zmail has the same action:**
  - `N`/`P` next and previous, `-`/Space page up and down;
  - `R` reply, with the reply-all question asked inline as Alpine does;
  - `F` forward, `S` save (move to a folder, with the folder prompt and
    Tab completion), `W` WhereIs (search), `*` flag;
  - `D` delete and `U` undelete.
  - Zmail deletes to Trash at once rather than marking for expunge, so `U`
    is the undo of that delete, and `X` (expunge) is not needed. `E`
    archives.
- **Compose is Pico-style:**
  - header fields (To, Cc, Bcc, Attchmnt, Subject), then the body;
  - Pico's control keys: `^X` send, `^O` postpone (save the draft), `^C`
    cancel, `^K`/`^U` cut and uncut line, `^J` attach, `^G` help, `^R` read
    a file into the body, `^W` where is, `^T` address book or file browser;
  - the command bar shows them.
  - `^J` needs the terminal's keyboard enhancement to tell it apart from
    Enter. Where a terminal can't, attach is also on `^A` and is shown in
    the bar.

**Modern touches:**

- Colour themes that follow the terminal's palette, with a monochrome
  fallback (`NO_COLOR`), and Unicode box drawing with an ASCII fallback.
- Mouse support: click a line to open it, scroll with the wheel, click
  command-bar entries.
- **A preview pane** at 120 columns and wider (`V` toggles it): the
  Message Index on the left and the selected message on the right. This
  is the one departure from Alpine's strictly full-screen views.
- Conversations: the index can group a thread (`/` toggles), with the
  message count and the newest date.
- Incremental WhereIs with the store's search syntax (`from:`, `has:`,
  `before:`), and the server-search fallback.
- Undo for every triage action (`U`, or `z` as an alias), not only delete.
- Relative dates ("10:42", "Yesterday", "Mon") and width-aware truncation
  of wide characters.
- Non-blocking feedback: sync progress, failures and "N new" go in the
  status line and never stop input.
- A command palette on `:` that lists every command with fuzzy matching,
  for people who don't know the letters.
- The keymap is data, so a user keymap file can rebind it.

### 3. Copy and paste, the way Zellij does it

Rob asked for Zellij's model: the app owns the mouse, selection stays
inside a pane, and copying goes to the system clipboard.

- **Selection.** With mouse capture on, a left-button drag selects text
  in the pane under the pointer, and only that pane, even when the
  preview pane is open beside it. Dragging past the top or bottom edge
  scrolls the pane. Double-click selects a word, triple-click a line.
  The selection is drawn in reverse video.
- **Copy on select.** Releasing the button copies the selection, as
  Zellij's `copy_on_select` does, and the status line says "Copied N
  characters". The copied text is taken from what the TUI drew, so it's
  already filtered (section 5). Wrapped lines rejoin as their original
  line and trailing padding is dropped.
- **Where it goes:**
  - **OSC 52** by default, as in Zellij. It works locally and over SSH in
    terminals that support it: iTerm2, kitty, WezTerm, Alacritty, foot,
    Windows Terminal, and tmux with `set-clipboard on`.
  - A `copy_command` setting, as in Zellij (e.g. `pbcopy`, `wl-copy`,
    `xclip -selection clipboard`). macOS Terminal.app has no OSC 52, so on
    macOS, when not over SSH, the default is `pbcopy`.
  - `copy_clipboard = "system" | "primary"` picks between the clipboard and
    the X11/Wayland primary selection.
- **The terminal's own selection.** Holding Shift while dragging bypasses
  the TUI and uses the terminal's own selection, as in Zellij. A setting
  turns mouse capture off completely.
- **Keyboard selection.** In Message Text, `Ctrl-Space` sets a mark, the
  arrow keys extend the selection, and `Ctrl-C`/`y` copies it. This is for
  people who don't use a mouse.
- **Paste.** Bracketed paste is on. A paste arrives as one block of text
  and goes into the focused field or the compose body literally; it
  never runs as commands. Outside a text field, a paste is ignored.

### 4. Reading mail

- The reader shows `PreparedMessage`'s text form: the Simple view. The
  formatted HTML view, remote images and inline images don't exist in the
  TUI.
- Links show as numbered references (`[3]`). Opening one shows the real
  URL and asks before handing it to the OS opener, as the GUI's link
  dialog does.
- Attachments are listed and saved through `AttachmentBlob::save_to`,
  with the sanitised file name (`attachment_safety`).

### 5. Terminal safety (required before any mail text is shown)

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
- The only escape sequences the TUI writes are its own drawing (through
  crossterm) and OSC 52 for a copy the user made (section 3). The OSC 52
  payload is the user's selection of already filtered text, base64-encoded.
  The TUI never emits OSC 8 hyperlinks.
- A hostile corpus test sits next to the HTML one. It covers CSI cursor
  moves, OSC 0/2 titles, OSC 52, OSC 8, DCS, 8-bit C1 introducers and
  bidi tricks, and asserts that no byte below 0x20 (except `\n`) and no
  ESC or C1 survives.

### 6. Writing mail

- Compose has a built-in Pico-style editor (section 2) on
  `ratatui-textarea`, the ratatui organisation's maintained successor to
  `tui-textarea`, which has had no release since October 2024. The draft is
  saved to the encrypted store as the GUI does. Send uses `SyncService::queue_draft_and_send(_after)`,
  so undo-send works the same way.
- `$EDITOR` is **opt-in** (Rob's decision). Handing the draft to an external
  editor means writing it as a plaintext temporary file, which ADR 0005
  otherwise rules out. If enabled, the file goes in a 0700 directory and
  is overwritten and deleted after the editor exits, and the setting
  says so.

### 7. Sync and notifications

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

### 8. What moves out of `zmail-ui`

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
| 1. Read | `zmail-tui`: unlock; Main Menu, Folder List, Message Index and Message Text with the command bar; preview pane; WhereIs; status line; refresh; mouse, Zellij-style copy and bracketed paste; `zmail-app tui` behind the `tui` feature | `TestBackend` snapshot tests for each view; manual check in a real terminal on each OS |
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
- **Clipboard support varies.** OSC 52 is missing in macOS Terminal.app,
  off by default in some terminals, and capped in size by others. Hence
  the `pbcopy` default on a local Mac, the `copy_command` setting, and the
  Shift-drag escape to the terminal's own selection.
- **Two frontends to keep in step.** New store features need TUI work
  too. Keeping the shared logic in `zmail-actions` limits that.
- **Accessibility.** Terminals depend on the screen reader's terminal
  support. The TUI keeps a linear, plain layout available.

## Decisions

Rob, October 31, 2026:

1. **Go ahead** with the TUI as proposed.
2. **Dependencies:** `ratatui`, `crossterm` and the textarea crate, "as long
   as they are currently maintained". Checked on October 31, 2026:
   - ratatui 0.30.2 (June 2026) and crossterm 0.29, both with commits in
     the last week;
   - `tui-textarea` has had no release since October 2024 and fails the
     condition, so its maintained successor `ratatui-textarea` (0.9.3,
     October 2026) is used instead.
3. **The TUI ships in every release:** the `tui` feature is on by default.
4. **External editor:** opt-in.
5. **One syncing process:** the sync lock is approved.
6. **Design:** Pine/Alpine rather than mutt, with cues from modern TUI
   apps (section 2), and copy and paste the way Zellij does it
   (section 3).
