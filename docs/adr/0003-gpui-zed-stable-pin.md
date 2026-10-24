# ADR 0003: Pin GPUI To Zed Stable Release Tags

## Status

Accepted on October 15, 2026 (issue #113).

Temporarily set aside in October 2026: for the macOS web renderer
(ADR 0006) GPUI is pinned to the head of Zed PR #62379 until a Zed stable
release contains it. See "Deviation From ADR 0003" in
[`docs/upstream/zed.md`](../upstream/zed.md).

## Context

Zmail's shell is built on GPUI. Until now it used `gpui 0.2.2` from
crates.io, published on October 22, 2025 from Zed commit `69e2130` and still
the newest release a year later. The UI plan (UI_PLAN.md, PR #111) needs
GPUI features that only exist in Zed's tree:

- AccessKit screen-reader support (Zed #56065, plus
  `Application::new_inaccessible` from #57954 for apps that opt out). This
  is the one capability Zmail cannot build itself.
- System notifications (#61189), Mica on Windows (#41842) and
  `WindowButtonLayout`, which follows the GNOME button layout.
- The wgpu Linux renderer that replaced blade.

The options were (a) a git pin on Zed, (b) staying on 0.2.2 until the end
of the UI work, which would mean porting about 3k lines of new components
afterwards, and (c) the third-party `gpui-pre` crate, a weekly snapshot
republished outside Zed and therefore a supply-chain concern.

## Decision

Zmail depends on GPUI through a git dependency on the Zed repository pinned
to a **Zed stable release tag** (`v1.24.2` at the time of writing), not an
arbitrary `main` commit:

- stable tags are what Zed ships to its own users and are followed by
  patch releases when something breaks;
- a weekly workflow (`.github/workflows/zed-upstream.yml`) opens a single
  `chore(deps): bump Zed GPUI to vX.Y.Z` pull request when a newer stable
  release appears, moving the toolchain and mirrored `[patch.crates-io]`
  entries with it;
- `deny.toml` allows exactly the git repositories the pinned graph uses.

The mechanics, the current pin and how to bump manually are in
[`docs/upstream/zed.md`](../upstream/zed.md).

## Consequences

API churn from 0.2.2 to `v1.24.2` was small for Zmail's code:

- The app starts from `gpui_platform::application()`; `Application::new()`
  is gone because the platform code moved into `gpui_macos`,
  `gpui_windows`, `gpui_linux` and `gpui_wgpu` (Zed #49277).
- `gpui::Timer` is gone; timers come from
  `cx.background_executor().timer(..)`.
- `Window::focus` takes the `App` context.
- `uniform_list(..).track_scroll` borrows its scroll handle.
- `Entity::update` on an async context no longer returns a `Result`; code
  that must notice a released view holds a `WeakEntity`.
- Assets (`AssetSource`) and text APIs used by Zmail were unchanged.

Costs, measured with clean release builds of `zmail-app` on the same Linux
host (12 cores, warm Cargo caches):

| | gpui 0.2.2 | Zed v1.24.2 |
| --- | --- | --- |
| Clean release build | 150-153 s | 152 s |
| Release binary | 43.0 MB | 60.1 MB (+40%) |
| Stripped binary | 34.0 MB | 44.6 MB (+31%) |
| gzip-compressed binary | 15.2 MB | 19.4 MB (+28%) |

The size increase comes mostly from wgpu (Vulkan and GL backends with the
naga shader compiler), AccessKit and the newer text stack. The first fetch
of the Zed repository takes about 400 MB of Cargo git database, which CI
caches.

The Rust toolchain now follows Zed's (`1.98.1` for `v1.24.2`; GPUI needs at
least 1.95), and CI reads it from `rust-toolchain.toml`.

On Linux the window renders through wgpu on Vulkan (lavapipe in headless
runs) with an EGL/GL fallback, so packages need `libvulkan1` or `libegl1` at
runtime. Text lays out through cosmic-text 0.19 instead of 0.14, which makes
some multi-line text blocks one pixel taller; there is no other visible
change.

Seven advisory exceptions that the old GPUI stack required (async-std,
instant, rustls-pemfile, core2, proc-macro-error2 and two quick-xml
advisories) no longer apply and were removed.
