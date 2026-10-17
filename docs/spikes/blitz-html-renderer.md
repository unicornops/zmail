# Spike: Blitz as the HTML mail renderer (issue #134)

Status: spike finished 2026-10-17. The go/no-go decision is Rob's; this document
is the evidence and a recommendation.

## Summary

**Recommendation: no-go for wiring Blitz into the reader now; go for a short
follow-up that fixes the blockers below and re-runs this corpus as the gate.**

What works:

- Blitz 0.3.0-beta.4 builds on our pinned toolchain (Rust 1.98.1, Blitz MSRV
  1.91) next to GPUI v1.24.2 in one workspace and one lockfile. Nothing in the
  existing lockfile changed version; there are no `links` conflicts.
  `cargo deny` and `cargo audit` stay clean with no policy change.
- Every resource load goes through our provider. With remote content blocked,
  nothing leaves the process, and `cid:` images come from the MIME parts.
- It is fast and small at runtime: a warm render takes 9.7 ms at p50 and
  14.2 ms at p95 on the corpus, and peak RSS is about 20 MB per render process.
- Fidelity is good when it works: 20 of 33 corpus mails match Chromium closely
  (grade A), including dark-mode media queries, Word/Outlook markup, RTL and
  Arabic shaping, and `cid:` images.

What blocks shipping it:

1. **Content loss.** 4 of 33 mails lose some or all of their content. A table
   cell or row whose `display` is changed from the table default (`td {
   display: block }`, the standard responsive-email stacking technique) lays
   out to zero height (minimal case below). A renderer that silently hides
   part of a message is worse than the text flattening we have now.
2. **Hostile input crashes it.** 91 of 1,504 mutated corpus mails (6%) panic.
   One line is enough: `<img src="https://a b.example/x.png">` panics in
   `blitz-dom` (`resolve_url` unwraps an unparseable URL). 1,000 nested tables
   overflow the stack, which aborts the process and cannot be caught.
3. **A filesystem read outside the provider.** With Blitz's `svg` feature on,
   usvg's default `<image href>` resolver reads any local path (on Windows a
   UNC path would reach the network). The spike leaves `svg` off; that is a
   mitigation, not a fix.
4. **Maturity.** Blitz is beta (4 betas since July 2026, 4.3k stars) and has
   effectively one maintainer: 1,884 of the top five contributors' 1,988
   commits are his. Its render stack churns: since the end of May `anyrender` went from
   0.11 to 0.14 and `vello_cpu` from 0.0.9 to 0.3, each a breaking release.

None of these look structural: 1 and the panics are bugs that are small to fix
upstream, 3 is a one-line resolver option, and the corpus harness here can
re-check a new Blitz release in minutes. Whatever the engine, faithful HTML
needs work zmail must do anyway: a style-preserving, parser-based sanitiser,
storing the HTML alternative and inline parts' `Content-ID`s, and an
out-of-process renderer (see "Integration").

## What was built

`crates/zmail-html-render` is a standalone crate that nothing depends on, so
`zmail-app` and the shipped binary are unchanged. It was built and checked as
a workspace member sharing the root `Cargo.lock` (the results in "Toolchain
and dependency fit"), then excluded from the workspace with its own
`Cargo.lock`: compiling Stylo in CI's quality job left the runner without disk
space for the live mail harness build. Re-adding it to `members` repeats the
GPUI compatibility check. Commands:

```sh
cargo test --manifest-path crates/zmail-html-render/Cargo.toml
cargo clippy --manifest-path crates/zmail-html-render/Cargo.toml --all-targets -- -D warnings
```

- `render(html, MailResources, RenderOptions) -> RenderedMail` parses the
  HTML, resolves style and layout at a CSS width and device scale, and
  rasterises the full document height with the CPU renderer (`vello_cpu` via
  `anyrender_vello_cpu`) into an RGBA buffer. `prefers-color-scheme` follows
  `RenderOptions::dark`. Engine panics are caught and returned as errors.
- `MailResources` is the only way to load anything. It answers `cid:` from
  inline parts (Content-ID matched case-insensitively, brackets optional) and
  `data:` URLs itself. `http(s)` is refused unless `RemotePolicy::Allowed`
  supplies a fetcher (none ships). Every other scheme (`file:`, relative URLs,
  `blocked://`) is refused. It also refuses web fonts unless allowed, image
  formats other than PNG/JPEG/GIF/WebP, images whose header declares more than
  24 MP, resources over 8 MB, and more than 256 loads per message. Refusals are
  listed on the result, ready for the "remote content blocked" banner.
- `RenderedMail::link_at(x, y)` maps a point in image pixels to the `href`
  under it (Blitz hit test, then the nearest `<a href>` ancestor), resolved
  against the message base, for the #118 "show the real target URL" flow.
- Examples: `render_mail` renders a `.html` or `.eml` file to PNG and prints
  timings, peak RSS, provider counts and an optional link probe;
  `bench_corpus` renders a directory repeatedly in one process.
- Tests: the provider refuses remote, `file:` and other schemes; `cid:`
  matching; `data:` decoding; fonts refused; decompression-bomb header refused;
  a table layout renders with the right pixels and its link is found; remote,
  `@import`, `file:`, missing `cid:` and relative loads never get past the
  provider; dark mode answers the media query; and SVG cannot paint a local
  file into the message (this test fails if the `svg` feature is turned on).

```sh
cargo run --release -p zmail-html-render --example render_mail -- \
    crates/zmail-html-render/corpus/32-synthetic-inline-cid-report.eml out.png \
    --width 600 --sanitizer keep-styles --link 300,200
cargo run --release -p zmail-html-render --example bench_corpus -- \
    crates/zmail-html-render/corpus 7 600
```

Three Blitz behaviours needed working around before anything rendered:

- `HtmlDocument::from_html` switches to the XML parser when it sees an XHTML
  doctype or namespace, which most bulk-mail templates carry. Mail is never
  well-formed XML, so the document came out empty. The spike drives
  `html5ever` with Blitz's own sink and always parses as HTML.
- A refused request must still complete. Blitz holds layout while a
  `<link rel=stylesheet>` in `<head>` is pending, and `NetHandler` has no
  error callback, so a silently dropped request left the page blank. The
  provider answers refusals with empty bytes.
- `blitz-dom` enables no image decoders; the embedder chooses them. In the
  app build, Cargo feature unification would also give it GPUI's decoders
  (EXR, TIFF, QOI and others), hence the format allowlist in the provider.

## Toolchain and dependency fit

| Item | Result |
| --- | --- |
| Crates | `blitz-dom`, `blitz-html`, `blitz-paint`, `blitz-traits` 0.3.0-beta.4 (2026-10-16); `anyrender` 0.14, `anyrender_vello_cpu` 0.18 (`vello_cpu` 0.3), `stylo` 0.22, `taffy` 0.15, `parley` 0.12 |
| MSRV | Blitz declares 1.91; we pin 1.98.1. Builds with `--locked`. |
| Next to GPUI v1.24.2 | Builds in the same workspace. No existing lockfile entry changed version. 77 lockfile entries are added: 70 new crates and second versions of 7 (`taffy` 0.13/0.15, `harfrust`, `phf` ×4, `synstructure`); `skrifa`/`read-fonts` were already duplicated by GPUI. |
| Native links | `links` keys shared with GPUI (`yeslogic-fontconfig-sys` 6.0.1, `rayon-core` 1.13) resolve to the same versions. The spike enables `fontique/fontconfig-dlopen` to load fontconfig at runtime as GPUI's font-kit does, so there is no new link-time dependency on Linux. |
| `cargo deny check licenses advisories bans sources` | Passes, no change to `deny.toml`. MPL-2.0 (Stylo, `cssparser`, `selectors`, `app_units`, 14 crates in all) is already allowed, as it is in `about.toml`. Other new licences: MIT, Apache-2.0, Unicode-3.0, BSD-3-Clause, Zlib, all allowed. All sources are crates.io. Duplicates are warnings under our policy. |
| `cargo audit` | No new advisories (the same three allowed warnings as `main`). |
| Build time and CI | Clean debug build of the crate 40 s, release 70 s (12 threads). As a workspace member, CI's Linux quality job passed format, clippy, `cargo deny`, `cargo audit` and the workspace tests with it, then ran out of disk building the live mail harness. Now excluded, CI does not build it at all; macOS and Windows builds of Blitz are unverified. As a product dependency it would add to every CI job's build time and disk. |
| Standalone `cargo deny` | With the repository's `deny.toml` against the crate's own lockfile: `advisories ok, bans ok, licenses ok, sources ok`. |
| Binary size (Linux x86_64 release, `zmail-app` with `render` called from `main`, measured locally, not committed) | 58.3 MB → 78.2 MB unstripped (+20.0 MB); 43.3 MB → 58.2 MB stripped (+14.9 MB, +34%). |

Licensing note: MPL-2.0 is file-level copyleft and explicitly compatible with
GPL-3.0 (MPL 2.0 section 3.3); linking Stylo into the GPL build needs nothing
beyond the notices `cargo about` already produces.

## Corpus and fidelity

33 messages, committed under `crates/zmail-html-render/corpus/` (520 KB of
HTML, no images): 26 MIT/Apache-2.0 templates (Cerberus, Lee Munroe's
responsive template, Mailgun, Postmark, sendwithus) and 7 synthetic ones
written for this spike, with sources, revisions and licence texts in
`corpus/ATTRIBUTION.md`. No real person's mail and no logos or other images
are included. A generated 1,500-row statement (not committed) is the stress
case.

Reference: Chromium 149 headless shell, 600 px wide, JavaScript disabled and
all network blocked, so remote images are missing on both sides. Only the
DejaVu fonts are installed on the test machine, so both engines use the same
fonts. Blitz rendered the same HTML with only `<script>` removed ("styles
kept", standing in for the style-preserving sanitiser faithful rendering
needs) and also the output of today's `sanitize_html_for_display`.

Grades: **A**, same structure, differences limited to font metrics and the
alt text of blocked images; **B**, all content present but visibly different;
**C**, content lost.

| # | Mail | Kind | Chromium height | Blitz height | Grade | Warm render (ms) | Notes |
| --- | --- | --- | ---: | ---: | :---: | ---: | --- |
| 01 | `cerberus-fluid` | newsletter | 1176 | 1141 | A | 14.4 | only alt text of blocked images missing |
| 02 | `cerberus-hybrid` | newsletter | 2205 | 2097 | B | 18.9 | hybrid columns stack differently; alt text missing |
| 03 | `cerberus-responsive` | newsletter | 2378 | 1137 | C | 14.1 | stacked `td{display:block}` sections lost |
| 04 | `leemunroe-responsive` | transactional | 366 | 395 | A | 8.6 | |
| 05 | `leemunroe-inlined` | transactional | 366 | 395 | A | 8.7 | |
| 06 | `mailgun-action` | transactional | 330 | 6 | C | 5.8 | `td.container{display:block}`: the whole body collapses to 6 px |
| 07 | `mailgun-alert` | transactional | 392 | 6 | C | 5.9 | same as 06 |
| 08 | `mailgun-billing` | receipt | 665 | 6 | C | 6.0 | same as 06 |
| 09 | `postmark-receipt` | receipt | 1291 | 1381 | A | 11.9 | |
| 10 | `postmark-invoice` | receipt | 1092 | 1158 | A | 10.4 | |
| 11 | `postmark-welcome` | transactional | 1210 | 1266 | A | 11.3 | |
| 12 | `postmark-password-reset` | transactional | 815 | 851 | A | 9.7 | |
| 13 | `postmark-comment-notification` | transactional | 485 | 517 | A | 8.5 | |
| 14 | `postmark-trial-expiring` | transactional | 1177 | 1227 | A | 12.3 | |
| 15 | `postmark-user-invitation` | transactional | 912 | 948 | A | 9.6 | |
| 16 | `sendwithus-mantra-receipt` | receipt | 1273 | 1444 | B | 12.7 | blocked hero and logo leave blank space, no alt text |
| 17 | `sendwithus-mantra-shipped` | transactional | 1061 | 1177 | B | 11.3 | as 16 |
| 18 | `sendwithus-meow-digest-left` | newsletter | 873 | 834 | A | 11.3 | |
| 19 | `sendwithus-meow-two-column` | newsletter | 586 | 528 | A | 10.5 | |
| 20 | `sendwithus-oxygen-invoice` | receipt | 1075 | 1177 | A | 9.6 | |
| 21 | `sendwithus-neopolitan-welcome` | marketing | 783 | 1070 | B | 8.6 | blocked images reserve large blank boxes |
| 22 | `sendwithus-skyline-progress` | marketing | 900 | 941 | B | 8.6 | step images and footer icons missing, no alt text |
| 23 | `sendwithus-sunday-survey` | marketing | 618 | 721 | B | 9.1 | rating image row missing, no alt text |
| 24 | `sendwithus-goldstar-update` | newsletter | 1117 | 1395 | B | 12.2 | italic heading rendered upright; icon row missing |
| 25 | `sendwithus-airmail-confirm` | transactional | 939 | 971 | A | 9.2 | |
| 26 | `sendwithus-go-upsell` | marketing | 641 | 552 | B | 8.8 | header image and alt text missing, footer thinner |
| 27 | `synthetic-outlook-word` | outlook | 461 | 471 | A | 11.2 | Word/MSO markup and conditional comments fine |
| 28 | `synthetic-dark-mode-newsletter` | newsletter | 904 | 904 | A | 12.0 | light and dark both match |
| 29 | `synthetic-reply-thread` | personal | 596 | 596 | A | 8.5 | nested `blockquote` quoting fine |
| 30 | `synthetic-intl-rtl` | intl | 845 | 845 | A | 8.9 | Hebrew/Arabic shaping and bidi match |
| 31 | `synthetic-float-layout` | newsletter | 610 | 670 | B | 8.3 | CSS floats fine; `<img align=right>` not floated |
| 32 | `synthetic-inline-cid-report` (`.eml`) | inline `cid:` | 408 | 428 | A | 10.0 (cold) | both `cid:` images from the MIME parts render; the missing one is refused |
| 33 | `synthetic-hostile-probe` | security | 843 | 853 | A | 10.7 | every load refused, nothing navigates |
| 34 | `synthetic-large-statement` (generated) | stress | — | 20000 (capped) | — | 214.3 | headless Chromium timed out on a 600×20000 screenshot |

Totals: 20 A, 9 B, 4 C.

Most B grades come from one gap: Blitz shows nothing for an image that failed
to load, where browsers draw the alt text. With remote content blocked by
default, that matters for mail. Allowing images would turn most B grades into
A. The other differences seen: no synthesised italic when the font has no
italic face, `align` not mapped on `<img>`, and darker default link colours in
dark mode.

Today's sanitiser output is not a usable input. `sanitize_html_for_display`
removes every `<style>` block, so media queries, dark mode and class-based
styling go; only templates whose styles are all inline (the Postmark ones
here) keep their look. It also rewrites `http(s)://` to `blocked://`
everywhere, including visible text (corpus 30 shows "blocked://links…" as
text, and the plain-text view shows the same), and it is string-based rather
than parser-based.

Minimal case for the content-loss bug (each prints a ~20 px tall page; Chromium
lays the 200 px box out):

```html
<table><tr><td style="display:block"><div style="height:200px">x</div></td></tr></table>
<table><tr style="display:block"><td><div style="height:200px">x</div></td></tr></table>
<div style="display:table"><div style="display:table-row"><div style="display:block">…</div></div></div>
```

Blitz does not wrap non-cell children of a table row in anonymous table
boxes, so their content is dropped. Related open upstream issues: `valign` and
`vertical-align` on table cells (#508), `height` not mapped on `tr`/`td`
(#507), phantom borders on collapsed tables (#504).

## Performance and memory

Measured on an AMD Ryzen 5 3600 (6 cores, 12 threads), release build, 600 CSS
px wide, scale 1 unless noted, corpus 01–33.

| Measure | p50 | p95 | max |
| --- | ---: | ---: | ---: |
| Warm render, in process (median of 7 runs per mail; the 32 HTML files) | 9.7 ms | 14.2 ms | 18.9 ms |
| Warm render, 800 px at 2× (1600 px wide image) | 12.1 ms | 20.8 ms | 24.3 ms |
| Cold render, one process per mail (parse/layout/raster) | 13.1 ms | 18.0 ms | 24.1 ms |
| of which parse incl. one-time font and Stylo setup | 7.7 ms | 8.8 ms | 8.9 ms |
| of which style and layout | 1.9 ms | 4.1 ms | 6.4 ms |
| of which raster | 3.1 ms | 5.9 ms | 8.8 ms |
| Peak RSS, one process per mail | 20.5 MB | 22.6 MB | 24.9 MB |

- The first render in a process costs about 8 ms extra (system font
  enumeration, Stylo setup); the process is at about 16 MB RSS after it.
- The 1,500-row statement takes 214 ms warm (110 ms layout, 105 ms raster)
  and peaks at 127 MB, of which 46 MB is the 600×20000 RGBA image; at 800 px
  and 2× it takes 295 ms. Rasterising the whole document is the wrong shape for long
  mail: paint only the visible tiles (`paint_scene` takes an offset and
  size) and the cost follows the viewport, not the message.
- #137 has no numeric budgets yet. Against the 16 ms frame budget it proposes
  for scrolling, one-off renders on a background executor fit easily; tiles
  would be needed for long mail.
- Robustness inputs (below) show the tails: a mutated newsletter took 2.7 s of
  layout and 624 MB peak RSS. A render needs a time and memory limit, which
  only a separate process can enforce.

## Security review

### What the engine can reach

| Surface | Finding |
| --- | --- |
| Network | None of its own. `blitz-*`, `anyrender*` and `vello_cpu` contain no socket, process or environment code; every `<img>`, stylesheet, `@import`, `@font-face`, `background-image` and `<iframe src>` becomes a `NetProvider::fetch` call (tested, and corpus 33 exercises each one). |
| Filesystem | Reads system fonts (fontconfig on Linux, CoreText on macOS, DirectWrite on Windows) through `fontique`. **With the `svg` feature on, usvg's default `<image href>` resolver reads any local path** from inline `<svg>`, SVG images and SVG backgrounds, bypassing the provider; verified by painting a local PNG into a render. On Windows a UNC path would make it an SMB request (credential leak). The spike builds without `svg` (`<svg>` is then simply not drawn) and a regression test guards this. Upstream fix: pass an `image_href_resolver` whose string resolver returns `None`. |
| Navigation and forms | No navigation or shell provider is installed and no events are dispatched, so links, forms and `<meta refresh>` are inert. Blitz's source shows `<iframe srcdoc>` creating nested documents (up to 10 deep) that load through the same provider (not exercised here); the sanitiser should drop iframes. |
| Parsers fed attacker bytes | `html5ever`, Stylo's CSS parser, Taffy and Parley layout, `harfrust` shaping, and the `image` decoders for PNG, JPEG (`zune-jpeg`), GIF and WebP. Fonts (`skrifa`, WOFF2 via `wuff` and Brotli) only if embedded fonts are allowed; they are off by default. |
| Unsafe code | The engine is not `forbid(unsafe_code)` as our crates are: `servo_arc` and `atomic_refcell` (Stylo's foundations) and parts of `blitz-dom` and `taffy` use `unsafe`. |
| Global state | Stylo keeps process-wide state (atoms, a rayon pool). The spike uses `StyleThreading::Sequential`; Blitz documents a panic when two parallel resolves overlap. |

### Parser robustness (measured)

1,504 inputs: 1,500 random mutations of the corpus (insertions of
mail-hostile fragments, deletions, duplications, byte flips) plus four
pathological documents, each rendered in its own process.

| Outcome | Count | Detail |
| --- | ---: | --- |
| Rendered | 1,410 | p50 31 ms, p95 155 ms, max 2.7 s (incl. process start) |
| Panic, caught by `catch_unwind` | 91 | 85 × `blitz-dom` `resolve_url` (unparseable URL in `src`/`href`); 5 × `unwrap` on an `@import` URL in `blitz-dom/src/net.rs`; 1 × assertion in `parley` line breaking |
| Stack overflow, process aborted | 1 | 5,000 nested tables. Thresholds on an 8 MB main-thread stack: 1,000 nested tables or 4,000 nested `div`s. |
| Over 20 s, killed | 1 | 100,000 nested `div`s |

### Crash isolation

In-process `catch_unwind` is not enough:

- Stack overflow aborts the process; nothing can catch it. A message with a
  thousand nested tables would take the whole app down.
- A panic can leave Stylo's global state or a `RefCell` borrowed; the next
  render may fail or misbehave.
- There is no way to stop a runaway layout or bound its memory from the same
  process.

Recommended: a renderer subprocess, the same `zmail-app` binary started with
a hidden subcommand (no extra binary or size). It reads sanitised HTML and
inline parts on stdin and writes tiles, link rectangles and text runs on
stdout. The parent kills it after a deadline (2 s, say) and restarts it after
any crash or every N renders; it is long-lived so warm renders stay near
10 ms. Limits: `RLIMIT_AS`/`RLIMIT_CPU` and a seccomp filter with no
network syscalls on Linux, a Job Object with a memory cap on Windows, and a
sandbox profile on macOS, where `RLIMIT_AS` is not enforced. Run the engine on a
thread with a large explicit stack, and cap DOM depth in the sanitiser (say
200) so the deep-nesting cases never reach it.

### Fuzzing plan

A `fuzz/` crate outside the workspace (cargo-fuzz needs nightly; the
workspace pins stable), run nightly in CI with `-rss_limit_mb=2048
-timeout=10`, seeded from `corpus/`:

1. `render_html`: arbitrary bytes → lossy UTF-8 → `render` at a fixed width.
   Panics, aborts, timeouts and RSS overruns are all findings.
2. `render_structured`: an `arbitrary`-driven generator of mail-shaped HTML
   (nested tables, `display` overrides, `align`/`valign`/`bgcolor`, `<style>`
   with media queries, dictionaries of email CSS properties).
3. `stylesheet`: arbitrary CSS in `<style>` over a fixed body, plus
   `@import` chains through a provider that serves `data:` CSS.
4. `provider`: `decode_data_url`, `cid:` normalisation and the image header
   guard; the guard must reject anything the full decoder would choke on.
5. `hit_test`: render, then `link_at` on arbitrary points.
6. Later, `sanitize_then_render` once the new sanitiser exists, asserting no
   provider request escapes the policy.

The 91 panics above came from a few minutes of naive mutation, so the first
night would be productive.

## Integration into the reader

Prerequisites, independent of the engine:

- **Sanitiser.** Parser-based (html5ever), keeps `<style>` and sanitises CSS
  (no `url()` outside `cid:`/`data:` image types, no `@import`, no
  `position: fixed`), drops `script`, `iframe`, `object`, `embed`, `form`
  controls, event attributes and `<base>`, caps nesting depth, and stops
  rewriting URLs in text. Blocking belongs in the provider.
- **Store.** The IMAP and JMAP adapters store HTML only when a message has
  no text/plain part, and stores the sanitised (URL-rewritten) HTML, so "load
  remote content" cannot be honoured later. Store the HTML alternative and
  sanitise at display time. `message_attachments` has no `Content-ID`; add
  it so `cid:` can be resolved from the local store.
- **Remote content.** A `RemoteFetcher` that goes through the sync layer's
  HTTP client with the user's per-message or per-sender choice, never from
  the UI thread, and caches results locally.

Display options:

| | Image (this spike) | Native GPUI elements |
| --- | --- | --- |
| How | Engine output shown as a GPUI image, painted in tiles as the reader scrolls; clicks hit-tested in the renderer. | Walk the laid-out Blitz tree and emit GPUI quads, images and text runs at Blitz's positions. |
| Fidelity | Exactly what the engine paints. | Text drawn by GPUI's text system, so glyph metrics drift from Blitz's layout. |
| Selection, find, copy | Lost unless rebuilt: the renderer can export text runs with positions to drive overlay selection and find-in-page highlights, but that is a separate project. | Possible, but GPUI has no cross-element text selection today (#118 plans one). |
| Accessibility | **Lost.** A screen reader sees one image. Blitz can build an AccessKit tree (its `accessibility` feature, off here); exposing it as a subtree of GPUI's AccessKit tree is unexplored. | Better, through GPUI's own AccessKit support. |
| Isolation | Fits a subprocess: only pixels and rectangles cross the boundary. | The layout tree has to cross the boundary; more IPC. |

An image is the right first step, but it should not become the default view
until selection and screen-reader access are addressed. The plain-text view
stays the fallback, "View plain text" keeps working, and the plain text
remains what search indexes.

## Fallback if no-go

A subset renderer to GPUI elements: html5ever plus a small CSS cascade
(inline styles and simple selectors, media queries for width and colour
scheme), mapping tables to GPUI's Taffy grid, blocks to flex, text to
`StyledText`/`InteractiveText` and `cid:` images through the same provider.
GPUI already lays out with Taffy, so table-to-grid is natural. It keeps
selection, links (#118) and accessibility in GPUI, is fully under our
control, and needs no MPL code. The costs: the CSS cascade and table
algorithm are real work, fidelity on complex marketing mail will be lower than
a browser engine, and dark mode and responsive stacking must be done by hand.
Either way, the provider, corpus and comparison harness from this spike carry
over unchanged.

## Proposed next steps

1. Report upstream (with the minimal cases above): anonymous table boxes,
   `resolve_url` and `@import` panics, usvg's file resolver, and alt text for
   failed images. Each fix is small. (Not filed yet; Rob's call.)
2. Write the parser-based, style-preserving sanitiser and the store changes;
   both are needed whatever engine is chosen.
3. Prototype the renderer subprocess with tiled output and limits.
4. Re-run this corpus and the mutation run on the next Blitz release. Gate:
   no C grades, no panics or aborts in the mutation run, `svg` safe to enable
   or left off.
