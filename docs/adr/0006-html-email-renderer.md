# ADR 0006: Rendering HTML Email Safely

## Status

Accepted on October 24, 2026; Rob's decisions are recorded under
"Decisions" at the end. Supersedes the Blitz spike (#134, PR #163),
which ended in a no-go.

## Context

### What Zmail does today

The reader never renders HTML. `sanitize_html_for_display`
(`crates/zmail-security`) works on strings: it drops `<script>` and
`<style>` blocks, rewrites every `http(s)://` inside a tag (links included)
to `blocked://`, flags remote content by substring, and turns the result
into text. `MessageRenderMode::SanitizedHtml` therefore means "text
derived from HTML": the reader lays it out as selectable paragraphs.

What users see: newsletters, receipts and most marketing mail as a wall
of text, table cells run together, no images, no colours, no `cid:`
inline images, and a "remote content blocked" banner with nothing that
could load it, because the adapters store the already-rewritten HTML
(only when there is no text part) and drop Content-IDs. #198 tracks the
lost link schemes and paragraph breaks.

The sanitiser is safe only because nothing renders its output. It keeps
event attributes, `iframe`, `object` and `base`, so it cannot be fed to
any renderer as it stands.

### What email HTML needs

Authors write for Gmail and classic Outlook, so mail is built from the
lowest common denominator. In order of how often it matters (sources:
caniemail.com; Litmus market share, September 2026):

1. Nested `<table>` layout with `width`, `align`, `valign`, `bgcolor`,
   `cellpadding`, `border`, `colspan` and `rowspan`.
2. Inline `style=""`: colour, font, padding, border, text alignment.
3. `<img>` with `width`/`height`, `cid:` images, and alt text when
   images are blocked.
4. `<style>` blocks with class selectors (78% client support; Gmail
   caps them at 16 KB).
5. `@media (max-width)` responsive stacking, usually `td { display:
   block }`.
6. Background colours and images (attribute and CSS).
7. Dark mode: `prefers-color-scheme`, `color-scheme`, and a forced-dark
   fallback for mail that ignores both.
8. Web fonts (`@font-face`): fall back to system fonts; they are remote
   loads and stay off by default.
9. To ignore: MSO conditional comments (parsed as comments), VML, AMP for
   email (`text/x-amp-html`), forms and scripts.

### Threat model

An email is hostile input from anyone on the internet, rendered
automatically when the user selects it. The renderer must defend
against:

- **Code execution through the engine**: memory-safety bugs in HTML,
  CSS, image and font parsing. Most browser-engine exploits need
  JavaScript; email must never run it.
- **Tracking and data leaks**: remote images, CSS `url()`, fonts,
  prefetch and DNS prefetch, `<a ping>`, meta refresh, forms. Loading
  any of them tells the sender the mail was opened and from where.
- **Local file and network access**: `file:` URLs, UNC paths on
  Windows, SVG `<image href>` (the Blitz spike found usvg's default
  resolver reads local files), requests to LAN hosts.
- **Deception**: links whose text shows one address and whose target is
  another, overlays that cover Zmail's own UI (sender, security banners),
  fake "verified" badges.
- **Denial of service**: deeply nested tables (the spike's 1,000-table
  mail overflowed the stack and aborted the process), huge images,
  pathological CSS. A bad mail must cost one message pane, never the
  app.

## Options considered

| Option | Security | Fidelity on real mail | Integration with GPUI | Verdict |
| --- | --- | --- | --- | --- |
| **OS web engines** (WKWebView, WebView2, WebKitGTK) | Vendor-sandboxed multi-process engines, patched by Apple, Microsoft and distros; JS off, every load blockable | Excellent: the engines mail is written for (Apple Mail, new Outlook, Geary, Evolution) | Hard: GPUI v1.24.2 cannot place a native view inside its window; no Wayland path | **Recommended**, behind a renderer boundary |
| **Servo** (the Rust engine Mozilla started) | Servo disclaims security guarantees for arbitrary content; its sandbox does not run on Apple Silicon or Windows; always links SpiderMonkey (C++) | Good (CSS 2 93.9%, tables 92.6% of WPT) | OpenGL-only output, CPU readback per frame; monthly breaking API | Not now; watch |
| **Blitz** (Stylo + Taffy + Parley) | Memory-safe, but the spike's blockers are unchanged on today's main: URL `panic!`, `@import` unwrap, stack overflow on nesting, local-file SVG resolver | Good when it works (20/33 close to Chromium); `td { display: block }` loses content | CPU raster into a GPUI image | Not now; watch |
| **In-house subset renderer** (html5ever, CSS subset, Taffy tables, GPUI text) | Smallest surface, memory-safe, no engine at all | About 80-90% of transactional mail, 60-70% of marketing mail (estimate) | Native: selection, find, screen readers all work | Kept as the safe fallback, not the main renderer |
| **litehtml** (C++) | C++ parser with recent out-of-bounds fixes, not fuzzed upstream | CSS 2 era, decent tables | Host-drawn | Rejected |
| **CEF** (Chromium) | Chromium's sandbox, but we would ship a Chromium security update every 4 weeks | Excellent | 100 MB+ per platform (one GPUI team abandoned it at 1 GB) | Rejected |
| **Ultralight, Sciter** | Closed source | | | Rejected: incompatible with GPL-3.0-only |

### Why the OS web engines

They are the only option that is strong on both axes Rob set: security
and rendering "the majority if not all" HTML mail well.

- **Security comes from the platform, not from us.** WebKit on macOS
  runs content in a sandboxed WebContent process; WebView2 renders in an
  AppContainer (since runtime 120); WebKitGTK has a bubblewrap sandbox
  (on by request in the 4.1 API, which we set). Apple, Microsoft and the
  distributions ship engine fixes without a Zmail release. With
  JavaScript off, the most common exploit route is closed. Apple Mail,
  Outlook, Geary and Evolution render untrusted mail this way.
- **Fidelity is the best available**, because senders test against
  these engines.

The cost is integration work, and a Linux story that is weaker than macOS
and Windows (below).

Servo and Blitz are the memory-safe future, and the renderer boundary
below is designed so one of them can replace the OS engines later. Today
both fail the security bar for content from strangers.

## Decision (proposed)

### 1. One renderer boundary, two renderers

A `MessageRenderer` boundary in a new `zmail-render` crate takes a
prepared message (sanitised HTML, the `cid:` parts, the user's
remote-content choice, the theme) and produces a view. Two
implementations:

- **Web**: the OS engine, the default wherever it is available.
- **Native**: the in-house subset renderer drawn with GPUI elements. It
  replaces today's text rendering, and is what Zmail uses when the web
  renderer is unavailable, fails, or is turned off ("Simple view" in
  Settings and per message), and as the accessible fallback.

### 2. Preparation is the same for every renderer

Before any engine sees a message:

1. **Store what renderers need**: the raw `text/html` part (even when a
   text part exists), the inline parts by Content-ID, and the original
   link targets. A store migration plus adapter changes for IMAP, JMAP
   and Proton.
2. **A real sanitiser**: parse with html5ever, then keep an allowlist of
   elements and attributes (ammonia, or our own pass on the same parser)
   that keeps `style`, tables and presentational attributes, and drops
   scripts, event handlers, `iframe`, `object`, `embed`, `base`, `form`,
   `meta http-equiv`, `link` and SVG. CSS (inline and `<style>`) goes
   through a CSS parser (lightningcss or cssparser):
   - drop `@import` and `@font-face`;
   - drop `position: fixed`;
   - rewrite `url()` to blocked placeholders unless the user allowed
     remote content;
   - cap sizes, nesting and the total node count, and refuse mail over
     the caps to the native renderer.
3. **Links are Zmail's**: every `href` is kept in a side table and
   replaced with an opaque id, so a click comes back to Zmail, which
   shows the real target with the existing look-alike warning (#198)
   before opening the browser. Engines never navigate.
4. **Remote content**: blocked by default, as now. "Load remote content"
   is per message, with "always for this sender" as an option. Loads go
   only to the remote origins the mail referenced, only over https, never
   to private or LAN addresses.

### 3. Web renderer hardening (every OS)

- **JavaScript off for mail content**: `allowsContentJavaScript = false`
  (WKWebView), `IsScriptEnabled = false` (WebView2),
  `enable-javascript-markup = false` (WebKitGTK). Zmail's own measuring
  script runs in an isolated world where the platform allows it.
- **Content served from a Zmail scheme** (`zmail-mail://`) with a CSP of
  `default-src 'none'; img-src zmail-mail: data:; style-src
  'unsafe-inline'; sandbox` (plus the allowed https origins once the user
  loads remote content). `cid:` resolves through the same scheme handler.
- **A second wall that blocks every load**: a WKContentRuleList,
  WebKitGTK's user content filter, and WebView2's
  `WebResourceRequested` filter on `*` that denies anything not served by
  the Zmail scheme or explicitly allowed.
- **Nothing else**: navigation, new windows, downloads, printing,
  context-menu "open link", drag-out and status bar are all denied or
  routed to Zmail. Each view uses an ephemeral, non-persistent data
  store: no cookies, no cache, no storage.
- **WebKitGTK**: sandbox enabled explicitly (wry leaves it off), WebGL
  and WebAudio off. A minimum WebKitGTK version is enforced; older
  systems get the native renderer.
- **Containment**: a crashed or hung content process costs one pane and
  falls back to the native renderer with a notice.

### 4. Putting the view in the window

GPUI v1.24.2 exposes the native window handle but cannot compose a native
child view. A view placed on top covers Zmail's menus, popovers and
dialogs, and cannot be clipped. Zed PR #62379 ("native window
composition": AppKit layers, DirectComposition visuals, Wayland
subsurfaces) addresses exactly this and is in review. The plan:

- **Build on #62379 now** (Rob's decision): the web renderer is
  developed and prototyped against GPUI from that PR's head, pinned by
  commit, so its composition support is tested with a real embedder today
  and problems are reported on the PR. `main` and normal releases pin
  that commit too (Rob, October 24, 2026: Zmail is alpha and may break
  between releases, so there are no preview builds; pre-releases come at
  stable). This is a recorded deviation from ADR 0003 until the first Zed
  release that contains the composition work, and the weekly bump
  workflow skips while the PR pin is in place.
- **No interim overlay-swap mode** is built. If #62379 stalls or changes
  shape, the snapshot swap described in the research remains the
  fallback, decided then.
- **Linux**: X11 gets the same native child view. Wayland has no way to
  put a WebKitGTK view inside a GPUI surface, so Wayland renders
  offscreen. A hidden WebKitGTK view draws the mail, Zmail shows a
  snapshot with a link hit map, and selection and find use the native
  renderer's text. If #62379 lands with WPE support this becomes a live
  view.

### 5. What does not change

- No JavaScript, ever, in mail content.
- Remote content off by default.
- The text view stays available (it is the native renderer's plainest
  output), and screen readers get the native renderer's structure where
  the web view's accessibility does not reach AccessKit (to be verified
  per OS).

## Plan

Each phase ships on its own.

| Phase | Work | Gate |
| --- | --- | --- |
| 0. Foundations | Store raw HTML, Content-IDs and link targets (migration and all three adapters); html5ever sanitiser with the CSS pass; link side table; fix #198. The reader still shows text, now with correct links and paragraphs. | Hostile-corpus tests: no script, handler, remote URL or `file:` survives; fuzzing in CI |
| 1. Prototypes | Six short spikes against GPUI from Zed PR #62379, each with a go or no-go: (1) WKWebView child view in GPUI on macOS through native composition (overlays above it, clipping, scrolling); (2) WebView2 child on Windows with DirectComposition kept on, or the cost of turning it off; (3) WebKitGTK offscreen snapshot on Wayland; (4) a canary HTTP server that must see zero hits from a hostile corpus on all three engines; (5) keyboard, focus and IME hand-off; (6) content height with page JS off | All six written up; Rob decides go |
| 2. macOS web renderer | Production WKWebView renderer behind the boundary (Rob's platform first), load remote content, dark mode, zoom to fit | Corpus renders compared against Apple Mail; canary test in CI |
| 3. Native renderer | The in-house subset renderer replaces text rendering everywhere (tables, inline CSS, images, dark mode) | Same corpus, side by side with the web renderer |
| 4. Windows | WebView2 renderer | As phase 2 |
| 5. Linux | WebKitGTK on X11 (child view) and Wayland (snapshot) | As phase 2 |
| 6. Upstream composition | `main`'s GPUI pin moves from the #62379 commit to the first Zed release that contains it (back to ADR 0003) | Overlays, clipping and scrolling right on all OSes |

### Status: Phase 4 (Windows)

The WebView2 renderer is `crates/zmail-render-web/src/platform/windows.rs`
(the only `unsafe` besides the macOS module) behind
`platform/windows_host.rs`:

- **In the window**: each message is a WebView2 *composition controller*
  whose root visual is a native composition surface of GPUI's
  DirectComposition tree, nested in one viewport surface that clips to the
  conversation list. DirectComposition stays on and Zmail's overlays draw
  above the page; there is no child window. A hosted visual gets no input,
  so the page's hitbox in GPUI forwards mouse buttons, moves, leaving and
  sideways wheel turns (`SendMouseInput`), and its cursor comes back
  (`CursorChanged`); vertical wheel turns scroll the conversation, as on
  macOS. Keyboard focus stays with GPUI (focus the page takes is handed
  back), and Copy and Select All reach the page through Zmail's commands
  (`ExecuteScript` for the selection, then the clipboard). GPUI's tree
  rebuild needs a workaround for nested native surfaces, see
  `docs/upstream/zed.md`.
- **Hardening** as in section 3: `IsScriptEnabled = false` (Zmail's own
  measuring script runs through `ExecuteScript`, which that does not
  stop); `zmail-mail:`, `cid:` and `zmail-link:` registered as custom
  schemes and served from `WebResourceRequested`, with a `*` filter that
  answers every other request with an empty 403; navigation cancelled
  except Zmail's page load, links reported to Zmail; frames never navigate;
  new windows, external schemes, downloads, permissions and basic
  authentication denied; no dev tools, context menus, web messages, host
  objects, status bar, zoom control, accelerator keys, autofill, password
  saving or SmartScreen lookups. The engine itself has no network: its
  proxy is a loopback port nothing listens on, because a cross-origin
  `<iframe>` made it open a connection before the cancelled navigation
  reached any handler, which disabling Chromium's predictors did not
  stop. Each page has its own InPrivate profile
  with strict tracking prevention, in a user data folder under the
  temporary folder that is deleted when the browser process exits (or on
  the next start). A failed browser or renderer process falls back to the
  text view.
- **Runtime**: the system's evergreen WebView2 (the loader is linked
  statically; nothing is added to the installer). Without it, or when the
  environment cannot be created, the reader keeps the text view and says
  why.

Effort, from the research: about 3-4 engineer-months for the web
renderer on macOS, Windows and Linux X11, plus about a month for the
Wayland snapshot path; the native renderer is about 3-4 months to a
first version, done alongside rather than after.

Re-evaluated yearly, or when either lands: Blitz with its panics,
anonymous table boxes and SVG resolver fixed; Servo with a sandbox on
every target and a stable embedding API. Either could replace the OS
engines behind the same boundary.

## Risks

- **GPUI composition**: #62379 is unmerged and may change before it
  lands; building on it means following its rebases, and its head is
  ahead of the Zed stable tag `main` uses. The Windows DirectComposition
  conflict may force a compromise. The prototypes decide this before any
  production work.
- **WebKitGTK patch lag**: stable distributions ran about 4 weeks behind
  WebKitGTK's last large security release (WSA-2026-0006, 233 CVEs).
  JavaScript off and no network narrow the exposure; Zmail shows a notice
  and uses the native renderer below a minimum version.
- **Accessibility** of web views through AccessKit is unverified on
  every OS; the native renderer is the fallback.
- **Wayland** gets a weaker experience (snapshot, native-renderer
  selection) until upstream composition supports it.
- **Engine bugs we cannot fix**: an OS engine bug waits for the vendor's
  patch.

## Decisions

Rob, October 24, 2026:

1. **Engine**: the OS web engines are the main renderer, with the
   in-house native renderer as the fallback and Simple view.
2. **GPUI composition**: build and test against Zed PR #62379 now and
   feed back on the PR, instead of an interim overlay-swap mode. `main`
   and normal releases carry the PR pin; no separate preview builds while
   Zmail is alpha.
3. **Wayland**: snapshot rendering, with selection and find from the
   native renderer, until upstream composition supports a live view.
4. **Remote content**: blocked by default; load per message, with
   "always for this sender".
5. **Order**: Phase 0, then the Phase 1 prototypes, then macOS (as
   proposed).

## References

- Servo: crate `servo` 0.7.0; LTS policy and security statement
  (servo/book `embedding/lts-release.md`); sandboxing in
  `components/constellation/sandboxing.rs`; WPT scores at
  https://wpt.servo.org.
- Blitz: DioxusLabs/blitz main as of 2026-10-24 (`document.rs`
  `resolve_url`, `net.rs` `@import`, `util.rs` usvg options; issues #781,
  #783, #786, #1006, #694).
- GPUI composition: zed-industries/zed PR #62379; gpui-kit
  `gpui-webview` docs and issues (#1787, #316, #2072, #1391, #765).
- WebView2 settings and distribution docs (Microsoft Learn); WebKitGTK
  sandbox (WebKit bug 220117); WSA-2026-0006 (oss-security,
  2026-09-29); wry `src/webkitgtk/mod.rs`.
- litehtml issues #491, #470, #387; CEF crate `cef` 156.0.0; longbridge
  `wef` (abandoned at 1 GB).
- caniemail.com (`html-style`, `prefers-color-scheme`); Litmus email
  client market share.
