# Zmail UI Modernisation Plan

Status: adopted. Written 2026-10-15 against `main` at v0.9.0 (`a030302`); section 1
describes that release. Progress as of 2026-10-18:

- Phase 0 (#114), Phase 1 (#115) and Phase 2 (#116) are done. Phases 3 to 8 are tracked
  in #117 to #122.
- Decision 1 is settled: option (a). GPUI now comes from the Zed v1.24.2 git tag (#125).
- Decisions 2, 3 and 4 are still open.

This replaces the first UI plan (icons, tokens, module split, light theme). That plan is
done: see "What the first plan delivered" at the end. This plan turns the working shell
into a modern, easy-to-use desktop email client on macOS, Windows, and Linux. It stays on
GPUI and keeps Zed theme compatibility.

---

## 1. Where we are

The evidence is:

- the v0.9.0 release screenshot (`zmail-linux-screenshot.png`)
- 22 further screenshots of a v0.9.0 release build: GreenMail with 15 varied messages,
  under Xvfb with lavapipe, at 900, 1440, and 1920px, in both themes, covering the empty
  state, account setup, reader, compose, search, drafts, and outbox
- a read-through of `crates/zmail-ui`
- a capability check of `gpui 0.2.2`

### What users see today

- **Debug text in the shell.** The window title and header say "Zmail Phase 5". The header
  also shows "Performance First", store row counts, "GPUI 0.2.2 • Zed 30ceb917 • 4 Tokio
  workers", queued op/job counts, and a hint to run `zmail-app dead-ops` (`toolbar.rs:105-169`).
  The account card shows the raw endpoint URL. The reader shows "Sync Status" and
  "Identities" sections, and a render-mode chip on every message. Compose shows "Draft 12",
  "Focus To/Subject/Body" buttons, and helper paragraphs on every card. Outbox rows read
  "pending • attempts=3".
- **A toolbar that overflows.** One row holds 10-11 bordered text buttons plus a labelled
  search card, with no overflow handling and no window minimum size. At 1440px it already
  runs off the right edge.
- **Broken thread rows.** Rows are 72px tall, but the content needs about 79px
  (`message_list.rs:163`), so the snippet spills into the next row. The avatar has no
  `flex_none` and gets squashed. Dates are raw RFC 3339 strings that get clipped.
- **Mailbox actions as a row of buttons.** "Move: X" is one button per mailbox
  (`reader.rs:383`). The reader action bar wraps onto two rows.
- **Banners that misbehave.** Success banners never clear. Errors are erased before they
  render when a thread is selected (`workspace.rs:290`, `message_list.rs:136`).
- **Fixed pane widths** (280 / 380 / flex). Nothing resizes or collapses, and there is no
  scrollbar anywhere.
- **Always dark.** The app starts in dark mode (`main.rs:1416`), ignores the OS appearance,
  and forgets the user's choice. Zed themes that use `#RRGGBBAA` colours (most real ones)
  silently fall back to the defaults (`styles.rs:363`). The theme mapping uses background
  tokens for `danger` and `warning` text colours.
- **Missing icon.** `save.svg` is referenced but not shipped.

### What the code can't do yet

- **No keyboard model.** There are no `actions!`, no `KeyBinding`, and no per-pane focus.
  Every key goes into whichever text field was clicked last, even after clicking a thread.
  `docs/phase5-hardening.md` says "all primary flows are keyboard reachable", which is not
  true.
- **Text input is a label with a `|` drawn in it.** The cursor is a byte index, so
  backspace or the arrow keys next to a non-ASCII character panic. Paste only listens for
  Cmd, so Ctrl+V does nothing on Linux and Windows. There is no selection, copy/cut,
  undo, key repeat, or IME.
- **Compose cursor jumps.** Every keystroke saves and reloads the draft, which moves the
  cursor to the end. The 5s refresh timer also wipes a half-typed attachment path.
- **Expensive hover.** One `hovered_element` field re-renders the whole workspace on every
  hover, and each render runs a `thread_view` DB query (`lib.rs:188`).
- **Missing interactions:** no context menus, tooltips, drag and drop, file picker,
  multi-select, undo, or loading states.
- **No UI tests.** There are 11 unit tests (hex parsing and string helpers) and no GPUI
  view tests.
- **No platform chrome.** There is no `cfg(target_os)` in zmail-ui, no macOS menu bar, and
  no custom title bar. On GNOME/Wayland, GPUI defaults to client-side decorations, so the
  window may have no title bar or resize border at all unless we draw them.

### What GPUI 0.2.2 gives us

`gpui 0.2.2` is still the latest crates.io release (2025-10-22). It was built from Zed
`69e2130`, **not** `30ceb917` as `docs/upstream/zed.md` records.

| Status | Capability |
|---|---|
| **In 0.2.2** | - Windows caption hit-testing via `window_control_area`, which also gives Snap Layouts on the maximise button<br>- macOS transparent title bar and traffic-light positioning<br>- Linux client-side decoration primitives: `request_decorations`, `start_window_move/resize`, `show_window_menu`, `set_client_inset`<br>- Blurred backgrounds: macOS vibrancy, Windows acrylic, KDE<br>- `observe_window_appearance` for light/dark switching<br>- Native file dialogs, dropping external files, clipboard (text and images)<br>- `uniform_list` and variable-height `list`<br>- `anchored` and `deferred` overlays, tooltips, animations<br>- Rounded `img`, ellipsis and `line_clamp`<br>- `EntityInputHandler` for IME<br>- `secondary-` key bindings, focus handles, tab order<br>- Element drag and drop<br>- Native menus on macOS only |
| **Needs newer gpui** | - Screen-reader support (AccessKit, Zed main since 2026-05)<br>- System notifications<br>- Mica<br>- GNOME window-button layout<br>- wgpu Linux renderer |
| **We build it** | - Title bars and window controls<br>- Linux client-side decoration frame (shadow, resize edges)<br>- Linux and Windows app menu<br>- Resizable panes, scrollbars, popovers and menus<br>- Text input<br>- Toasts |

`longbridge/gpui-component` has most of these widgets. Versions 0.5.1 and older target
`gpui 0.2.2`, but that line is no longer maintained. Version 0.6 and later switched to
`gpui-pre`, a third-party snapshot of Zed's gpui. It also brings heavy dependencies
(tree-sitter, lsp-types, html5ever) and its own `Root` and `Theme` globals. **Decision:**
build a small in-house component set. Port files from gpui-component 0.5.1 (Apache-2.0)
and Zed (`platform_title_bar`, `examples/input.rs`, `examples/window_shadow.rs`) where
that saves time, with attribution in `about.toml`.

---

## 2. Design direction

### Principles

1. **Mail first, chrome second.** The list and the message get the space. Diagnostics move
   to Settings → About. Status is one quiet line, and an error is an actionable row on the
   account it affects.
2. **Instant and local.** Every action reads and writes the local mirror and is visible in
   under 100ms. Undo replaces confirmation dialogs.
3. **Keyboard complete.** Every action has a command, a shortcut, and a menu entry, and
   shows up in the command palette.
4. **Native where it shows.** Window controls, menu model, modifier keys, fonts, radii,
   send-button placement, and light/dark follow each platform. Everything else is one
   cross-platform design.
5. **Secure by default, quietly.** Blocked remote content and sender-safety warnings are
   short, tiered banners with one plain sentence, not chips everywhere.

### The cross-platform core (most modern clients agree)

- **Layout:** three panes (collapsible sidebar, message list, reader) with resizable,
  persisted widths. Below about 1100px the sidebar becomes an overlay. Below about 760px
  the window shows one pane at a time, with a back button.
- **Sidebar:** Unified Inbox and per-account inboxes, then Favourites (Flagged, Drafts,
  Sent, Outbox), then a collapsible folder tree for each account. Unread badges appear on
  inboxes and favourites only. Each account can have a colour.
- **Message rows:** two densities.
  - *Comfortable* has three lines: avatar, then bold sender if unread, with the date on
    the right; then the subject; then a one-line snippet.
  - *Compact* is one line: sender · subject — snippet · date.
  - Rows also show an unread dot, a thread-count pill, and attachment and flag icons.
  - On hover, quick actions (Archive, Delete, Flag, Read) replace the date.
- **Dates:** "10:42" today, "Yesterday", then "Mon" within the week, "12 Oct" this year,
  and "12 Oct 2024" for older mail. They use the locale's 12/24-hour setting, show the full
  timestamp as a tooltip, and refresh at midnight.
- **Selection:** a full accent row when the list has focus, grey when it doesn't. Cmd/Ctrl
  and Shift extend the selection, and multi-select shows a bulk action bar.
- **Reader:**
  - A large subject.
  - A header on each message: avatar, sender name (address in a tooltip), "to me, Alice
    +2" (expandable), and the date.
  - Read messages collapse to one line; the latest and unread messages are expanded.
    Quoted text folds away.
  - Icon actions with tooltips: Reply, Reply All, Forward | Archive, Delete, Flag, More.
  - Attachment cards.
- **Compose:** a separate window for new mail, and inline reply with a pop-out button.
  - A From menu appears only when there is more than one identity.
  - Recipients are chips with autocomplete; Cc and Bcc hide behind toggles.
  - Secondary+Enter sends. Undo Send defaults to 10s.
- **Undo toasts** for archive, delete, move, and mark actions, plus Secondary+Z.
- **Command palette** on Secondary+K. An optional Gmail keymap adds j/k, e, #, r/a/f, s,
  c, z, g-i, `/`, and `?`.
- **Follow the system** for light/dark, with an override saved in Settings.

### Per-platform deltas

These all live in one `PlatformStyle` value, chosen once at startup with `cfg(target_os)`.
Views read it; they never branch on the OS themselves.

| | macOS | Windows 11 | Linux (GNOME/KDE) |
|---|---|---|---|
| Window chrome | `appears_transparent` with toolbar content beside the traffic lights | Custom 40px title bar with Segoe Fluent glyph buttons tagged `WindowControlArea::{Min,Max,Close}` (gives Snap Layouts) | Client-side decorations when the compositor gives `Decorations::Client`: drawn header bar, controls filtered by `window_controls()`, shadow and resize edges via `set_client_inset`. Fall back to server-side decorations (KDE, X11 without a compositor). |
| Menus | Native menu bar via `cx.set_menus`, listing every command | Overflow (⋯) menu at the end of the title bar, built from the same menu model | Hamburger menu at the end of the header bar, built from the same menu model |
| Primary modifier | Cmd (`secondary-`) | Ctrl | Ctrl |
| UI font | `.SystemUIFont` (SF Pro) | `.SystemUIFont` (Segoe UI Variable) | Bundled **Inter** registered via `text_system().add_fonts()`. GPUI's Linux system-font lookup resolves to an IBM Plex Sans that it doesn't ship. Inter matches GNOME 48's Adwaita Sans. |
| Radii (window / control) | 10 / 6 | 8 / 4 (0 when maximised) | 12 / 6 |
| Send button | Window toolbar, trailing | Bottom-left of the compose body | Header bar, trailing |
| Search field | Toolbar, trailing | Title bar, centred | Header bar, above the message list |
| Backdrop | Optional vibrancy on the sidebar (`Blurred`) | Opaque (no Mica until gpui is upgraded) | Opaque |
| Scrollbars | Overlay, auto-hide (`should_auto_hide_scrollbars`) | Thin, widen on hover | Overlay, auto-hide |

---

## 3. Decisions needed before Phase 2

1. **When to upgrade GPUI.** AccessKit (screen readers) is the one thing we cannot
   hand-build on 0.2.2, and a mail client has to be usable with a screen reader. Upgrading
   also brings system notifications, Mica, GNOME button layout, and the wgpu Linux
   renderer. Options:
   - **(a) Upgrade first (recommended).** Do a time-boxed spike that pins a Zed git
     revision, with an `allow-git` entry in `deny.toml`. Do it before building the
     component set, so we don't write about 3k lines of components twice.
   - **(b)** Build on 0.2.2 now and migrate at the end.
   - **(c)** Adopt `gpui-pre` from crates.io. It is a third-party snapshot, which is a
     supply-chain concern.
2. **Plain-text or rich compose.** This plan keeps plain-text compose plus Markdown-style
   shortcuts. A rich-text editor would be a separate project.
3. **OAuth provider buttons.** One-click "Sign in with Google/Microsoft" needs zmail-owned
   OAuth client IDs. Google's restricted Gmail scope means a CASA security assessment.
   Until that's settled, provider buttons pre-fill server settings and keep the client-ID
   fields under "Advanced" (see #89).
4. **Account autodiscovery privacy.** An ISPDB lookup (autoconfig.thunderbird.net) sends
   the user's mail domain to Mozilla. Proposal: try DNS SRV (RFC 6186/8314) and the
   domain's own `autoconfig` first, then ask before using ISPDB.

---

## 4. Phases

Each phase is a series of PR-sized slices. Every slice must pass:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Each phase also refreshes the screenshot matrix (Phase 1.8) and attaches it to the PR.

### Phase 0: Stop the bleeding (small, independent PRs, no new infrastructure)

1. **Remove debug text.** Window title becomes "Zmail". Drop the header diagnostics, the
   endpoint URL, internal draft and job IDs, the render-mode chip, the reader's Sync Status
   and Identities sections, the "Focus *" buttons, and the helper paragraphs. Keep the data
   reachable for Settings → About (Phase 6).
2. **Fix thread-row layout.**
   - Put the row height in one constant derived from the line heights, and add a unit test
     that the content fits.
   - Mark the avatar and date `flex_none`. Give the text column `min_w_0()`, and use
     `truncate()` / `line_clamp(1)`.
   - Apply the same fixes to search, draft, and outbox rows.
3. **Readable dates.** Add `zmail_core::time::format_list_date(ts, now, locale)` (chrono is
   already a workspace dependency), with tests for each tier, year boundaries, the
   12/24-hour setting, and time zones. Use it in the list and reader. Schedule a
   re-render at midnight.
4. **Sender display.** Move `sender_identity()` from `reader.rs` into core. Rows show the
   display name, and multi-participant threads show "Alice, Bob (3)". Replace the
   "a -> b" participant string built in zmail-imap with structured participants.
5. **Banners.** Stop clearing `store_error` during render. Status messages become toasts
   that auto-dismiss after 4s; errors persist until dismissed. Drop the "Issue: " prefix.
6. **Theme fixes.**
   - Parse `#RRGGBBAA` (blending alpha over the background).
   - Map `danger`, `warning`, and `success` from Zed's `error`, `warning`, and `success`
     foreground tokens.
   - Read the button, warning, and success backgrounds from the theme instead of
     hard-coded values.
   - Add the missing `save.svg`.
7. **Window basics.** Add `window_min_size` (720×480), `app_id` `ie.unicornops.zmail` (the
   macOS bundle ID; rename `zmail.desktop` to match and add `StartupWMClass`, or Wayland
   docks won't associate the window with its icon), and an `observe_window_appearance` hook that
   follows the system until the user overrides it.
8. **Cheap hover.** Replace `hovered_element` plus `cx.notify()` with GPUI's `.hover(|s| …)`
   style refinements. Cache `thread_view` per selection, not per frame.
9. **Pane sizing.**
   - The list pane is `flex_none`. Today it shrinks from 380px to about 250px depending on
     reader content.
   - The reader and compose panes get `min_w_0` and wrapping, so long lines and cards stop
     overflowing off the right edge.
   - The search field gets a fixed width.
   - The account-setup inputs stop sizing to their placeholder text; with OAuth2 selected,
     the right column currently goes off-screen.
10. **Small correctness fixes seen in screenshots.**
    - Plurals ("1 accounts", "1 identities", "1 messages").
    - Mailbox rows show one number, the unread badge, not "13 [13]".
    - The selected-mailbox highlight follows Drafts, Outbox, and Search modes.
    - Opening a message marks it read, after a short delay as in other clients.
    - Compose shows the identity once.
    - The JMAP endpoint placeholder doesn't say `imaps://`.
    - Validation errors appear beside the field.
    - Compose with no account explains why account setup opened.
11. **IMAP HTML bodies (functional bug, separate `fix:` PR).**
    - `hydrate_bodies` (`zmail-imap/src/lib.rs:1322`) prefers mail-parser's `body_text(0)`,
      which mail-parser synthesises from the HTML part when there is no text part. So
      IMAP HTML-only mail is shown flattened, with words run together, and
      `has_remote_content` is never set.
    - The remote-content banner in Phase 4 therefore can't work for IMAP until this is
      fixed.
    - Fix: check whether a real `text/plain` part exists (`text_body` vs `html_body`
      indices) before falling back to the HTML branch, and add a test with an HTML-only
      fixture.
12. **Docs.** Correct `docs/upstream/zed.md` to `69e2130`. Rewrite the accessibility section
   of `docs/phase5-hardening.md` to describe what is actually true today.

### Phase 1: Foundations (decision 1 lands first: upgrade spike, then components)

1. **GPUI upgrade spike** (if decision 1a). Pin a Zed revision, port the `Application`,
   `Window`, and asset APIs, and get CI green on all three OSes. Run the
   live-harness screenshot. Record the new pin in `docs/upstream/zed.md`.
2. **Split the view tree into entities.**
   - `WorkspaceView` owns `SidebarView`, `MessageListView`, `ReaderView`, and
     `ComposeView`. Each has its own `FocusHandle` and `key_context` ("Sidebar",
     "MessageList", "Reader", "Compose", "TextInput").
   - Settings becomes its own window entity.
   - Mutations go through one `MailActions` service that writes the store, emits
     `MailEvent`s, and registers undo entries.
3. **Actions and keymap.** Add `actions!` for every command, a default keymap in
   `crates/zmail-ui/keymaps/default.json` using `secondary-`, and pane cycling with Tab and
   F6. Arrow keys work within panes, and Esc closes overlays. Every toolbar button
   dispatches an action instead of calling methods directly.
4. **`TextInput` component.** Port GPUI `examples/input.rs`:
   - `EntityInputHandler` for IME.
   - Grapheme-safe cursor using `unicode-segmentation`.
   - Selection with mouse and Shift, word and line movement, copy/cut/paste on
     `secondary-`, undo/redo, key repeat, and placeholder text.
   - A multi-line variant for the compose body.
   - Tests cover non-ASCII editing (the current panic), paste on Ctrl, and IME commit.
   - Delete `UiBuffer`.
5. **Primitives in `crates/zmail-ui/src/components/`:**
   - `Button` (primary, subtle, ghost, danger) and `IconButton` (with tooltip)
   - `Tooltip`
   - `Popover`, `Menu` (also used for context menus), and `Select`
   - `Scrollbar` (overlay, auto-hide)
   - `SplitHandle` (resizable panes using an `on_drag`/`on_drag_move` marker)
   - `Toast` host (with an undo action)
   - `Modal` / `Sheet`
   - `Badge`, `Avatar` (`flex_none`; initials now, image later), `Kbd`, and a `Skeleton`
     row
   - Focus-ring styling that meets WCAG 2.2: 2px, at 3:1 contrast against the unfocused
     state.
6. **Tokens 2.0.**
   - A real type scale (caption, body, body-strong, title, headline) defined in `rem`, so
     the OS text-scaling setting works.
   - Spacing and radius tokens used everywhere. `PlatformStyle` from section 2.
   - Density tokens: row heights and paddings for comfortable and compact.
   - An automated contrast test over both bundled themes: 4.5:1 for text, 3:1 for icons
     and focus.
7. **UI preferences store.** Persist theme mode, density, reading-pane position, pane
   widths, sidebar collapse, last mailbox, keymap, and undo-send delay in `zmail-core`
   config (not in browser-style ad-hoc files).
8. **Test harness.**
   - Enable `gpui`'s `test-support` feature in dev-dependencies.
   - Add `VisualTestContext` tests for keyboard navigation, row layout fit, input editing,
     and undo.
   - Extend `scripts/capture-release-screenshot.sh` into a **screenshot matrix**: empty
     state, list, reader, compose, settings, light theme, 900px and 1920px windows.
     Upload it as a CI artefact on UI PRs.

### Phase 2: Window chrome and shell (per-platform)

1. **`TitleBar` component** (port of Zed's `platform_title_bar`):
   - **macOS:** transparent title bar with traffic-light padding.
   - **Windows:** custom glyph buttons and drag region, hover and pressed states, and
     radius 0 when maximised.
   - **Linux:** draw controls only when the compositor gives `Decorations::Client`;
     right-click opens `show_window_menu`; double-click maximises.
2. **Linux CSD frame.** Shadow, rounded corners, and resize edges (`window_shadow.rs`
   pattern), switched off when tiled or maximised. Test on GNOME/Wayland, KDE/Wayland, and
   X11 with and without a compositor.
3. **Toolbar redesign.** The title bar holds:
   - *leading:* sidebar toggle and Compose (primary)
   - *centre:* search (or list-pane header on Linux)
   - *trailing:* a sync indicator (spinner when syncing, warning icon on error, tooltip
     "Updated 2 min ago") and the overflow or hamburger menu (Windows/Linux)

   Message actions move to the reader header. Drafts and Outbox move into the sidebar.
   Theme moves to Settings and the View menu.
4. **Menus.** One `MenuModel`:
   - App/File: New Message, New Window, Settings, Quit
   - Edit
   - View: density, reading pane, theme, sidebar
   - Mailbox: Sync, New Folder
   - Message: Reply, Reply All, Forward, Archive, Delete, Move to…, Flag, Mark Read/Unread
   - Help: Keyboard Shortcuts, About, Diagnostics

   It renders natively on macOS and as the overflow menu elsewhere. Every entry shows its
   binding.
5. **Fonts.** Bundle Inter (OFL-1.1, add it to the licence inventory) for Linux. Keep
   system fonts on macOS and Windows.
6. **Responsive layout.** Resizable panes with persisted widths. Below 1100px the sidebar
   becomes an overlay; below 760px the window shows one pane with a back button.
   Optional bottom reading pane (View menu).

### Phase 3: Sidebar and message list

Store dependencies are marked **[store]**.

1. **Mailbox roles in the model [store].**
   - Add `role` (inbox, drafts, sent, archive, junk, trash) to `MailboxSummary`; it is
     already stored (`zmail-store` `mailbox_id_by_role`).
   - Have zmail-imap use SPECIAL-USE (RFC 6154) before guessing from names.
   - Icons and sorting come from the role, not the name.
   - Add a mailbox hierarchy (delimiter and parent) for the folder tree.
2. **Sidebar.**
   - Unified Inbox **[store: cross-account thread query]**.
   - Favourites: Flagged, Drafts, Sent, and Outbox (Outbox shows "3 waiting" and a retry
     action).
   - A collapsible per-account tree, with the account header showing a colour dot, name,
     and status icon. Errors get a "Fix…" action that opens Settings → Accounts.
   - The whole sidebar scrolls as one list.
   - Folders accept dropped messages.
3. **Message list.**
   - A header with the mailbox name, an unread count, and a filter menu (All / Unread /
     Flagged / Has attachments).
   - Comfortable and compact rows, as described in section 2.
   - Hover quick actions.
   - Selection model: focused and unfocused states, Cmd/Ctrl-click, Shift-click,
     Shift+arrows, and select all.
   - Bulk action bar.
   - Right-click context menu.
   - Drag rows to folders (`on_drag` with a `DraggedThreads` payload and a count preview).
4. **Undo.** `MailActions` records the inverse for archive, delete, move, flag, and mark
   actions. Toast with "Undo" plus Secondary+Z. Focus moves to the next row after a row
   leaves the list.
5. **Loading and empty states.**
   - Skeleton rows during the first sync of a mailbox.
   - "You're all caught up" for an empty inbox.
   - "No results in Inbox — Search all mail" for a search with no results.
   - Each empty state has one action.
6. **Drafts and Outbox rows** reuse the message row. Outbox status reads in plain words:
   "Sending…", "Failed — retrying in 5 min", "Retry now".

### Phase 4: Reader

1. **Conversation as a variable-height `list`.** Read messages collapse to a one-line
   summary; the latest and unread messages are expanded. Quoted text and signatures fold
   behind a "…" toggle. Keyboard: n/p to move between messages, Enter to expand.
2. **Message header.** Avatar, sender name (address in a tooltip, with a copy action),
   recipients summarised as "to me, Alice +2" and expandable, a relative date with the full
   date in a tooltip, and per-message Reply and More.
3. **Actions.** Icon buttons with tooltips, as listed in section 2. The More menu holds
   Move to… (a searchable folder picker that replaces the "Move: X" buttons), Mark Unread,
   View plain text / source, Save attachments, and Print (later).
4. **Banners.**
   - Remote content: a neutral one-line notice. Loading remote content stays out of scope
     until there is an HTML engine and a security review.
   - Sender safety (#108): tiered banners (info, caution, danger), each with one plain
     sentence and an action ("Looks safe", "Show details").
   - The chips go away.
5. **Attachments.** Cards with a type icon, file name, and human-readable size (e.g.
   "1.2 MB"). Per-file Open (`open_with_system` after saving to the cache) and Save As
   (`prompt_for_new_path`), plus Save All.
6. **Body.**
   - Readable line length (max about 72ch), proper paragraph spacing, and selectable text.
     Selection is a custom element over `StyledText`, which GPUI lacks.
   - Links rendered with `InteractiveText`. Clicking shows the real target URL before
     opening, and mismatched link text gets a warning.

### Phase 5: Compose

1. **Compose window.** `cx.open_window` per draft for new mail. Inline reply at the bottom
   of the conversation, with a pop-out button. Closing or pressing Esc keeps the draft
   (autosaved).
2. **Fields.**
   - The From menu appears only when there is more than one identity.
   - To, Cc, and Bcc use recipient chips. Autocomplete comes from a frecency-ranked
     address history **[store: collect addresses from mirrored and sent mail]**.
     Backspace removes a chip, invalid addresses are highlighted, and Cc/Bcc hide behind
     toggles.
   - Subject.
   - The body uses the multi-line `TextInput`.
3. **Attachments.** Use `prompt_for_paths` and drop external files onto the window
   (`ExternalPaths`). Show cards with remove buttons. The free-text "Attachment Path" field
   goes away.
4. **Send.** Platform placement from `PlatformStyle`; Secondary+Enter.
   - **Undo Send:** the outbox job is scheduled `undo_send_delay` seconds ahead (10s by
     default). It shows a toast with Undo, which cancels the job and reopens the draft.
     This reuses the existing `next_attempt_at`.
5. **Autosave without cursor jumps.** Debounce saves to 1s, and never rebuild
   `ComposeState` from the store while the editor has focus.

### Phase 6: Settings and onboarding

1. **Settings window** with these sections:
   - Accounts: list, add, remove, re-authenticate, colour, and cache budget.
   - Appearance: System/Light/Dark, density, reading pane, and importing a Zed theme file.
   - Keyboard: standard or Gmail keymap, and a shortcut list.
   - Privacy: remote content, sender-safety strictness.
   - Sending: undo-send delay.
   - About & Diagnostics: version, GPUI/Zed pin, queues, dead-lettered operations with
     Retry and Discard buttons (replacing the `zmail-app dead-ops` hint), and a "Copy
     diagnostics" button.
2. **First-run onboarding.**
   - Welcome, then provider choice: Google, Microsoft 365, Fastmail (JMAP), iCloud, or
     Other.
   - Then email address and password, or OAuth.
   - Then autodiscovery **[core: new `zmail-core::autodiscover`; DNS SRV → domain
     autoconfig → ISPDB with consent (decision 4) → MX guess]**.
   - A manual form only as a fallback, using `Select` for security (instead of the
     free-text "plain, starttls, or implicit_tls") and separate host and port fields
     (instead of an `imaps://` URL).
   - Finish by choosing an account colour and running the first sync, with progress.

### Phase 7: Keyboard-first and search

1. **Command palette** (Secondary+K). Fuzzy search over all actions, folders ("Go to
   Archive"), and accounts. Each entry shows its shortcut.
2. **Optional Gmail keymap**, plus a `?` cheat-sheet overlay.
3. **Search.**
   - `/` or Secondary+F focuses search. Search runs live (debounced 150ms) against local
     full-text search. A clear button and Esc exit.
   - Scope chips: this mailbox / all mail / all accounts.
   - Filter chips: from:, has:attachment, unread, date.
   - Results use the normal rows with matches highlighted.

### Phase 8: Accessibility and polish

1. **Screen readers** (needs the upgraded gpui). Roles and labels for every component. Rows
   are announced as "Unread, from Alice, Lunch on Friday, 10:42, 2 attachments". Live
   regions for toasts.
2. **Reduced motion** (macOS, Windows, GNOME settings): fades or instant changes instead of
   slides.
3. **Text scaling** via `rem_size`. Check that layouts reflow at 200%. Hit targets are at
   least 24×24.
4. **Windows high-contrast mode:** fall back to system colours where the platform reports
   them.
5. **Performance budget:** list scroll at 120fps with 100k threads (existing `benches/`),
   and under 100ms from action to visible change. Add a bench for opening the reader.

---

## 5. Order and parallelism

```
Phase 0 ──► Phase 1 (upgrade spike → entities/actions → TextInput + components → tests)
                 │
                 ├─► Phase 2 (chrome)  ─┐
                 ├─► Phase 3 (list)     ├─► Phase 7 (palette/search) ─► Phase 8
                 ├─► Phase 4 (reader)   │
                 └─► Phase 5 (compose) ─┘
Phase 6 (settings/onboarding) can start after Phase 1.5 (Modal, Select, TextInput).
```

Store and core dependencies can run in parallel with Phase 1:

- mailbox roles and hierarchy
- the cross-account thread query
- address history
- autodiscovery
- inverse operations for undo

## 6. Out of scope here

- Rendering HTML mail faithfully, and loading remote content. The Blitz spike (#134,
  #163) was a no-go for now.
- Rich-text compose (decision 2).
- Calendar and contacts.
- Smart categorised inboxes (Primary/Updates/…) — possibly later, and opt-in.
- Mobile.

---

## What the first plan delivered

Lucide icons via `zmail-assets`, `UiColors` semantic tokens, the module split of
`zmail-ui`, hover states on the toolbar, sidebar, and rows, a bundled light theme, and
empty states. The first plan's choices that this plan reverses:

- hover tracking through one `hovered_element` field (replaced by GPUI `.hover()` styles
  for performance)
- a single `WorkspaceView` entity (replaced by per-pane entities, needed for focus,
  keymaps, and multiple windows)
