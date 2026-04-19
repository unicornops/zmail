# Zmail UI Improvement Plan

## Context

Zmail's UI is functional but visually bare — all controls are text labels, there are no icons, no hover states, no visual hierarchy, and the entire UI lives in a single 2,923-line file. This plan transforms the interface into a polished, modern desktop email client while preserving performance, security, and Zed theme compatibility.

The current UI is in `crates/zmail-ui/src/lib.rs`. The theme system is in `crates/zmail-theme/src/lib.rs`. The app entry point is `crates/zmail-app/src/main.rs`. GPUI 0.2.2 provides native `svg()` element support via the `AssetSource` trait, with usvg/resvg rendering under the hood.

---

## Step 1: Icon System with Lucide SVGs

**Goal:** Embed Lucide icons and create a GPUI `AssetSource` so `svg()` elements render icons.

**Why Lucide:** ISC license (already in `deny.toml` allow-list). Clean 24x24 single-path SVGs. Covers all email client needs.

1. **Add `rust-embed` to workspace dependencies** in `/Cargo.toml`:
   ```toml
   rust-embed = { version = "8", features = ["interpolate-folder-path"] }
   ```

2. **Create `assets/icons/` directory.** Download ~35 Lucide SVG files:
   - Navigation: `mail.svg`, `inbox.svg`, `send.svg`, `search.svg`, `archive.svg`, `trash-2.svg`, `star.svg`, `refresh-cw.svg`, `plus.svg`, `settings.svg`, `chevron-down.svg`, `chevron-right.svg`, `x.svg`
   - Compose: `pen-square.svg`, `paperclip.svg`
   - Messages: `mail-open.svg`, `reply.svg`, `reply-all.svg`, `forward.svg`, `flag.svg`, `eye.svg`, `eye-off.svg`
   - Accounts: `user.svg`, `user-plus.svg`, `server.svg`, `shield.svg`, `key.svg`
   - Status: `check-circle.svg`, `alert-circle.svg`, `loader.svg`, `clock.svg`, `wifi.svg`, `wifi-off.svg`
   - Misc: `folder.svg`, `tag.svg`, `file-text.svg`, `sun.svg`, `moon.svg`

3. **Create `crates/zmail-assets/` crate.** Add to workspace members. Implements `gpui::AssetSource` using `rust-embed`:

   - `crates/zmail-assets/Cargo.toml` — depends on `gpui` and `rust-embed`
   - `crates/zmail-assets/src/lib.rs` — `#[derive(RustEmbed)] #[folder = "../../assets/icons"] #[prefix = "icons/"]` struct, implements `AssetSource::load()` and `AssetSource::list()`

4. **Wire into the app.** In `crates/zmail-app/src/main.rs`, change `launch_gui()`:
   - Add `zmail-assets` dependency to `crates/zmail-app/Cargo.toml`
   - Pass `ZmailAssets` to `zmail_ui::run()`

5. **Update `zmail_ui::run()` signature** to accept `impl AssetSource`. Change `Application::new().run(...)` to `Application::new().with_assets(asset_source).run(...)`.

6. **Add `icon()` helper** in the UI crate:
   ```rust
   fn icon(name: &str, size: f32, color: u32) -> gpui::Svg {
       svg().path(format!("icons/{name}.svg")).size(px(size)).text_color(rgb(color))
   }
   ```

**Files:** `Cargo.toml`, `crates/zmail-assets/Cargo.toml`, `crates/zmail-assets/src/lib.rs`, `crates/zmail-app/Cargo.toml`, `crates/zmail-app/src/main.rs`, `crates/zmail-ui/src/lib.rs`, `assets/icons/*.svg`

**Verify:** `cargo build -p zmail-app --release --locked && cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings`

---

## Step 2: Design Tokens and Style Constants

**Goal:** Replace hardcoded color literals and magic pixel numbers with a structured style system.

1. **Expand `SemanticTheme`** in `crates/zmail-theme/src/lib.rs`. Add fields: `selected_background`, `hover_background`, `danger`, `warning`, `success`, `info`. Update `from_theme()` to extract from Zed tokens (`element.hover`, `error`, `warning`, `success`, `info`) with sensible fallbacks.

2. **Expand `ThemeSummary`** in `crates/zmail-core/src/lib.rs` to carry the new tokens.

3. **Create `crates/zmail-ui/src/styles.rs`** with:
   - Spacing constants (`SPACE_1` through `SPACE_8`, based on 4px grid)
   - Layout dimensions (`SIDEBAR_WIDTH: 280`, `MESSAGE_LIST_WIDTH: 380`, `TOOLBAR_HEIGHT: 48`, `LIST_ITEM_HEIGHT: 72`, `MAILBOX_ITEM_HEIGHT: 40`, `AVATAR_SIZE: 32`, `ICON_SIZE_SM/MD/LG: 16/20/24`, `BADGE_SIZE: 20`)
   - Border radii (`RADIUS_SM: 4`, `RADIUS_MD: 6`, `RADIUS_LG: 8`, `RADIUS_FULL: 9999`)
   - `dark` and `light` fallback color modules
   - `UiColors` struct computed from theme at render time — replaces the ad-hoc `let background = ...` locals in `render()`

4. **Update `render()`** (line 1040) to construct `UiColors` from `self.bootstrap.theme`. Pass `&UiColors` to all sub-render functions instead of individual color params.

**Files:** `crates/zmail-theme/src/lib.rs`, `crates/zmail-core/src/lib.rs`, `crates/zmail-ui/src/styles.rs`, `crates/zmail-ui/src/lib.rs`

**Verify:** Same as Step 1. UI looks identical.

---

## Step 3: Component Decomposition

**Goal:** Break the monolithic `lib.rs` into focused modules. Pure refactor — no visual changes.

Target file structure for `crates/zmail-ui/src/`:

| File | Content |
|------|---------|
| `lib.rs` | `run()`, module declarations, `WorkspaceView` struct + `Render` impl, `ListMode`, `DetailMode` |
| `styles.rs` | From Step 2 |
| `workspace.rs` | `impl WorkspaceView` methods: refresh, selection, sync, search, compose, account setup, key handling |
| `toolbar.rs` | `render_toolbar()` function |
| `sidebar.rs` | `render_sidebar()` — accounts + mailbox list |
| `message_list.rs` | `render_message_list()` — threads, search, drafts, outbox |
| `reader.rs` | `render_reader_detail()`, `reader_action_buttons()` |
| `compose.rs` | `render_compose_detail()`, `compose_focus_buttons()`, `ComposeState` |
| `account_setup.rs` | `render_account_setup_detail()`, `account_protocol_buttons()`, `AccountSetupState` |
| `widgets.rs` | `icon()`, `toolbar_button()`, `input_card()`, `empty_state()`, `list_row()`, `badge()`, `avatar()`, `hex_to_rgb()` |
| `input.rs` | `UiBuffer`, `EditorField`, key handling |

**Pattern:** Each render function takes `&WorkspaceView` (or relevant state slices) + `&UiColors` + `&mut Window` + `&mut Context<WorkspaceView>` and returns `impl IntoElement`. `WorkspaceView` stays as the single GPUI entity (plain functions, not `cx.new()` entities).

**Verify:** `cargo build && cargo test && cargo clippy`. UI identical.

---

## Step 4: Toolbar and Sidebar Visual Improvements

**Goal:** Add icons to toolbar, improve sidebar with folder icons, unread badges, hover states.

1. **Redesign `toolbar_button()`** to accept an optional icon name. Renders `[icon] [label]` with `rounded_md()`, padding, and hover background. Each button needs a unique `.id()` (derive from label).

2. **Add hover state tracking.** Add `hovered_element: Option<SharedString>` field to `WorkspaceView`. Use `.on_hover()` to set/clear. During render, compare button ID to choose background color.

3. **Map toolbar buttons to icons:**
   - Add Account → `plus`
   - Compose → `pen-square`
   - Mail → `mail`
   - Drafts → `file-text`
   - Outbox → `send`
   - Search → `search`
   - Sync → `refresh-cw`

4. **Sidebar accounts section:**
   - Add `avatar()` widget (first letter of display name in a colored circle, `AVATAR_SIZE` px, `rounded_full()`)
   - Show sync phase badge with icons (`check-circle` Idle, `loader` Syncing, `alert-circle` Failed)
   - Hover background on account items

5. **Sidebar mailbox list:**
   - Add folder icons based on name matching: "Inbox" → `inbox`, "Sent"/"Sent Items" → `send`, "Drafts" → `file-text`, "Archive" → `archive`, "Trash"/"Junk"/"Spam" → `trash-2`, default → `folder`
   - Add `badge()` widget: small pill with accent bg, shown only when `unread_threads > 0`
   - Reduce row height from 48px to `MAILBOX_ITEM_HEIGHT` (40px)
   - Hover state on each row

6. **Create `badge()` and `avatar()` widgets** in `widgets.rs`:
   - `badge(count, bg_color, text_color)` → rounded pill with `.text_xs()`
   - `avatar(name, size, bg_color)` → circle with initial letter

**Files:** `crates/zmail-ui/src/widgets.rs`, `crates/zmail-ui/src/toolbar.rs`, `crates/zmail-ui/src/sidebar.rs`, `crates/zmail-ui/src/lib.rs` (add `hovered_element` field)

**Verify:** Build and run. Toolbar icons visible. Sidebar has folder icons and unread badges. Hover states work.

---

## Step 5: Message List and Reader Visual Improvements

**Goal:** Add sender avatars, unread styling, attachment/flag indicators to message list. Improve reader header and action bar.

1. **Redesign message list rows:**
   - Add sender `avatar()` on the left (deterministic color from hash of sender name)
   - Unread threads: subject in brighter foreground (or bold via `.font_weight()`), small blue dot indicator
   - Flagged threads: `star` icon in warning color
   - Attachment indicator: `paperclip` icon after subject
   - Layout: `[Avatar] [Subject + icons] [Date]` / `[Sender]` / `[Preview (muted, truncated)]`
   - Row height: `LIST_ITEM_HEIGHT` (72px) instead of 82px

2. **Improve reader pane header:**
   - Large subject line
   - Participants, date, mailbox in a row with proper spacing
   - Badges for flags, unread status

3. **Add reader action bar** (icon buttons at top of reader, before scroll area):
   - Reply (`reply`), Reply All (`reply-all`), Forward (`forward`)
   - Separator
   - Archive (`archive`), Delete (`trash-2`)
   - Separator
   - Flag (`flag`), Mark Read/Unread (`eye`/`eye-off`)
   - Simplify the existing bottom action buttons (keep Move-to-mailbox buttons only)

4. **Improve message cards in thread view:**
   - Sender avatar on left
   - Sender name bold, email in muted
   - Date right-aligned
   - Subtle divider between messages
   - Proper padding on message body

**Files:** `crates/zmail-ui/src/message_list.rs`, `crates/zmail-ui/src/reader.rs`, `crates/zmail-ui/src/widgets.rs`

**Verify:** Build and run. Message list shows avatars and indicators. Reader has proper header and action bar.

---

## Step 6: Compose, Account Setup, and Empty States

**Goal:** Polish remaining views.

1. **Improve `input_card()`:**
   - `rounded_md()` on borders
   - Brighter accent border when focused (focus ring effect)
   - Placeholder text in muted color when empty and not focused
      
2. **Improve compose layout:**
   - Identity selector row with `chevron-down` icon
   - Clean vertical field stack with consistent labels
   - Attachment list as small cards with file icon, name, and `x` remove button
   - Prominent accent "Send" button with `send` icon
   - Secondary "Save Draft" button

3. **Improve account setup layout:**
   - Section headers: "Connection", "Identity", "Outgoing Mail (SMTP)"
   - Protocol selector as toggle buttons with `server` icon
   - Related fields grouped in subtle background cards

4. **Improve empty states:**
   - Large muted icon centered: no accounts → `user-plus`, no mail → `inbox`, no search results → `search`, no drafts → `pen-square`, no message selected → `mail-open`
   - Larger title, smaller detail text
   - Optional action button (e.g., "Add Account" in no-accounts state)

5. **Improve status/error bar:**
   - Success: green left border + `check-circle` icon
   - Error: red left border + `alert-circle` icon

**Files:** `crates/zmail-ui/src/compose.rs`, `crates/zmail-ui/src/account_setup.rs`, `crates/zmail-ui/src/widgets.rs`

**Verify:** Build and run. Forms look polished. Empty states show icons. Status messages have visual indicators.

---

## Step 7: Light Theme

**Goal:** Add a light theme and toggle button.

1. **Create `themes/phase0-light.json`** following the Zed v0.2.0 schema:
   - background: `#FFFFFF`, surface: `#F8FAFC`, border: `#E2E8F0`, text: `#1E293B`, text.muted: `#64748B`, accent: `#2563EB`

2. **Add light fallback colors** to `crates/zmail-ui/src/styles.rs` in a `light` module.

3. **Update `crates/zmail-app/src/main.rs`** to load both themes. Bundle the light theme with a second `include_str!()`. Parse both into `SemanticTheme` variants. Pass both to `AppBootstrap`.

4. **Add theme toggle to toolbar:** Button with `sun`/`moon` icon. When clicked, swap `self.bootstrap.theme` to the alternate variant, rebuild `UiColors`, call `cx.notify()`.

5. **Verify icons adapt** — SVG fill uses `text_color`, which comes from theme tokens, so icons auto-adapt.

**Files:** `themes/phase0-light.json`, `crates/zmail-ui/src/styles.rs`, `crates/zmail-app/src/main.rs`, `crates/zmail-core/src/lib.rs` (expand `AppBootstrap`), `crates/zmail-ui/src/toolbar.rs`

**Verify:** Build and run. Toggle button switches between dark and light. All colors, text, and icons remain readable in both modes.

---

## Step 8: Polish and Final Verification

**Goal:** Clean up remaining rough edges.

1. **Audit hardcoded colors:** Search for `0x` hex literals in UI code. Replace with `UiColors` fields.

2. **Audit magic pixel numbers:** Search for `px(` with raw numbers. Replace with `styles::` constants.

3. **Add `rounded_md()` / `rounded_full()`** consistently: message cards, input fields, avatars, badges, toolbar buttons.

4. **Typography hierarchy:**
   - Section headers: default size, bold
   - List item titles: default size
   - Subtitles: `.text_sm()`, muted
   - Preview text: `.text_sm()`
   - Metadata/badges: `.text_xs()`, muted

5. **Full verification suite:**
   ```bash
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --locked -- -D warnings
   cargo test --workspace --locked
   cargo build -p zmail-app --release --locked
   cargo deny check licenses advisories bans sources
   ```

6. **Manual testing checklist:**
   - Launch with no accounts → empty state with `user-plus` icon
   - Add an account → form with section headers and grouped fields
   - After sync → sidebar shows mailboxes with folder icons and unread badges
   - Select a thread → reader shows avatar, header, action bar with icon buttons
   - Compose → form fields with focus rings, identity selector, attachment cards
   - Toggle dark/light → all colors adapt, icons remain visible
   - Resize window → no overflow or clipping (flex layout)
   - Hover toolbar/sidebar items → subtle background highlight

---

## Dependency Summary

| Step | Depends On |
|------|-----------|
| Step 1 (Icons) | None |
| Step 2 (Tokens) | None |
| Step 3 (Decomposition) | Step 2 |
| Step 4 (Toolbar + Sidebar) | Steps 1, 2, 3 |
| Step 5 (Messages + Reader) | Step 4 |
| Step 6 (Compose + Empty States) | Step 4 |
| Step 7 (Light Theme) | Steps 2, 3 |
| Step 8 (Polish) | All above |

Steps 5, 6, and 7 can run in parallel after Step 4 completes.

## Key Technical Decisions

- **Lucide icons (ISC)** — already in deny.toml allow-list, clean SVGs, comprehensive coverage
- **`rust-embed` (MIT)** — compile-time embedding, satisfies GPUI's `AssetSource` trait
- **Module decomposition via functions, not GPUI entities** — plain functions returning `impl IntoElement`, matching Zed's own patterns; `WorkspaceView` remains the single entity
- **Hover tracking via `hovered_element: Option<SharedString>`** — simple approach that avoids custom element types
- **Theme switching at runtime** — both variants loaded at startup, toggle swaps `UiColors` source
