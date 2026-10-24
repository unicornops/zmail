# Zed Upstream Record

Zmail's UI is built on GPUI, Zed's UI framework. GPUI is not consumed from
crates.io: the workspace pins it to a Zed commit through a git dependency.
ADR 0003 says that commit is a **Zed stable release tag**, and a scheduled
workflow proposes a bump whenever Zed ships a newer stable release (issue
#113). **For now Zmail deviates from that**: see below.

## Deviation From ADR 0003: Zed PR #62379

Since October 2026 the `gpui` and `gpui_platform` lines pin GPUI by commit
to the head of [Zed PR #62379](https://github.com/zed-industries/zed/pull/62379)
("native window composition"), an unmerged pull request from a fork. Rob
decided this for ADR 0006: the macOS web renderer for HTML mail needs
GPUI to place a native `WKWebView` between its base and overlay surfaces,
which no Zed release can do yet. Zmail is alpha software, so the renderer
ships in normal releases built on this pin instead of a preview build.

| | |
| --- | --- |
| Pull request | [zed-industries/zed#62379](https://github.com/zed-industries/zed/pull/62379), open |
| Repository | [`huacnlee/zed`](https://github.com/huacnlee/zed), branch `window-composition-tree` |
| Commit | [`840e095adddab3b7a644b137c22254fb2d111491`](https://github.com/huacnlee/zed/commit/840e095adddab3b7a644b137c22254fb2d111491) (committed 2026-09-27, based on Zed `main` of that time) |
| Rust toolchain (the PR's `rust-toolchain.toml`) | `1.98.1`, the same as `v1.24.2` |

What changes while the pin is on the pull request:

- `Cargo.toml`: both lines use `git = "https://github.com/huacnlee/zed"`
  with the same `rev`. `deny.toml` allows the fork.
- `[patch.crates-io]` follows the PR's root manifest: `async-task` is
  unchanged; the PR's manifest names `calloop` without a revision, so the
  patch pins the commit its `Cargo.lock` resolves
  (`eb6b4fd17b9af5ecc226546bdd04185391b3e265`).
- Zmail needed no source changes for this pin. Only
  `crates/zmail-render-web` uses the composition API
  (`Window::enable_window_composition`,
  `WindowComposition::create_native_surface`), on macOS. Linux and Windows
  build on the same pin and keep the text view.
- The app's diagnostics strip shows the Zed tag as `untagged` (the pin has
  no tag) and the PR commit.
- The weekly `zed-upstream` workflow does not move the pin back to a
  stable tag: `scripts/bump-zed-gpui.sh` sees the PR pin and asks GitHub
  whether #62379 is merged and which commit it merged as. While it is
  unmerged, or merged but in no stable release yet, the script stops with
  a notice. Once a Zed stable release contains the merge commit, it bumps
  to that release as usual, which ends the deviation; then delete this
  section and the `deny.toml` entry for the fork.

To follow a new head of the pull request by hand: change the `rev` on both
lines, run `cargo update -p gpui -p gpui_platform`, compare the PR's root
`[patch.crates-io]` and `Cargo.lock` entries for `async-task` and `calloop`
with ours, update the table above, and fix any API breakage in the same
pull request.

## Current Pin

<!-- zed-pin:start -->
| | |
| --- | --- |
| Zed pull request | [#62379](https://github.com/zed-industries/zed/pull/62379) (unmerged; see the deviation above) |
| Commit | [`840e095adddab3b7a644b137c22254fb2d111491`](https://github.com/huacnlee/zed/commit/840e095adddab3b7a644b137c22254fb2d111491) |
| Last stable tag before it | [`v1.24.2`](https://github.com/zed-industries/zed/releases/tag/v1.24.2) (`fb69067729cb16a7f6135e2c1600c26a28c59659`, 2026-10-14) |
| Rust toolchain (the PR's `rust-toolchain.toml`) | `1.98.1` |
<!-- zed-pin:end -->

The table between the `zed-pin` markers is rewritten by
`scripts/bump-zed-gpui.sh` when it bumps to a stable release; keep the
markers. `Cargo.lock` is the source of
truth for the exact commit, and the app's diagnostics strip reads the tag and
commit from it at build time (`crates/zmail-app/build.rs`).

Other upstream inputs:

- Theme schema used by Zmail: `https://zed.dev/schema/themes/v0.2.0.json`,
  implemented by the local compatibility layer in `crates/zmail-theme`.

## What The Pin Consists Of

- `Cargo.toml` `[workspace.dependencies]`: `gpui` and `gpui_platform`, both
  `git = "https://github.com/zed-industries/zed"` with the same `tag` (on
  the #62379 pin: the fork's URL with the same `rev`).
  `gpui_platform` is the crate that now picks the OS backend
  (`gpui_platform::application()`); since Zed #49277 the platform code lives
  in `gpui_macos`, `gpui_windows`, `gpui_linux` and `gpui_wgpu`, and an app
  can no longer start from `gpui::Application::new()`.
- `Cargo.toml` `[patch.crates-io]`: Zed's root manifest patches some
  crates.io crates, but `[patch]` only applies to the workspace that
  declares it. Zmail repeats the two that change GPUI's runtime behaviour,
  `async-task` (GPUI's executor) and `calloop` (the Linux event loop), at
  the revisions Zed uses. The bump script re-syncs them.
- `rust-toolchain.toml` and `rust-version`: the toolchain Zed pins at that
  tag. GPUI follows new stable Rust quickly (v1.24.2 needs at least 1.95
  for `std::hint::cold_path`), so tracking Zed's toolchain keeps the
  build on what upstream tests. CI reads the channel from
  `rust-toolchain.toml`.
- `deny.toml` `allow-git`: exactly the git repositories the pinned graph
  uses (Zed itself, its `font-kit`, `xim-rs` and `wasm_thread` forks, the
  `proptest` revision GPUI's `test-support` feature needs for `zmail-ui`'s
  view tests, and the two patch sources).

## Why Stable Release Tags

- crates.io stopped tracking GPUI: `gpui 0.2.2` (2025-10-22) is still the
  newest release a year later, and lacks AccessKit screen-reader support,
  system notifications, Mica, `WindowButtonLayout` and the wgpu Linux
  renderer that the UI plan needs.
- Arbitrary `main` commits are not tested as a release; a stable tag is what
  Zed ships to its own users, gets a changelog, and is followed by patch
  releases when something breaks.
- The third-party `gpui-pre` crate (weekly snapshots republished by someone
  outside Zed) was rejected as a supply-chain risk.

## Automatic Bumps

`.github/workflows/zed-upstream.yml` runs every Monday and on demand
(`workflow_dispatch`, optionally with a specific tag). It calls
`scripts/bump-zed-gpui.sh`, which:

1. reads the latest Zed release from
   `gh api repos/zed-industries/zed/releases/latest` (never a draft or
   pre-release) and stops when it is not newer than the pinned tag;
2. moves the `gpui`/`gpui_platform` tag, `rust-toolchain.toml`,
   `rust-version` and the mirrored patches to what Zed uses at the new tag;
3. runs `cargo update -p gpui -p gpui_platform`, falling back to a full
   `cargo update` only when the targeted update cannot resolve;
4. rewrites the pin table above.

The workflow then force-pushes the result to the `chore/zed-gpui-bump`
branch and opens, or updates, a single pull request titled
`chore(deps): bump Zed GPUI to vX.Y.Z` with links to the release notes of
every stable release since the current pin. `chore` keeps these bumps out of
the release changelog; a bump that needs real code changes should be merged
with a `feat`/`fix` title instead.

The workflow pushes and opens the PR with `secrets.RELEASE_PLEASE_TOKEN`
when it is set, like `release-please.yml`. Without it, it falls back to the
default `GITHUB_TOKEN`, and pull requests opened with that token do not
trigger CI: close and reopen the PR, or push to its branch, to run the
checks. The bump never edits `.github/workflows`, so the token needs no
`workflows` scope.

## Bumping Manually

```bash
scripts/bump-zed-gpui.sh            # latest stable release
scripts/bump-zed-gpui.sh v1.25.0    # a specific stable tag
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check licenses advisories bans sources
cargo build -p zmail-app --release --locked
```

The script needs an authenticated `gh` CLI, `cargo` and Python 3.11 or
newer. Then launch the app (or run `scripts/capture-release-screenshot.sh`
under Xvfb) to check that it still renders, and fix any GPUI API breakage on
the same branch.

## History

- **Phase 0 (March 20, 2026):** the crates.io `gpui 0.2.2` release. This
  record previously named Zed commit `30ceb9176cb751b0f4f60516a3248f4ded99b082`
  (February 27, 2026) as the GPUI reference, but that commit only declared
  the same crate version; the published crate's `.cargo_vcs_info.json` shows
  it was built from
  [`69e2130295c2649963eb639fc70b4f2ee8ea1624`](https://github.com/zed-industries/zed/commit/69e2130295c2649963eb639fc70b4f2ee8ea1624)
  ("gpui 0.2.2", October 22, 2025). That is the code Zmail ran until the
  move to Zed tags.
- **October 2026 (issue #113):** moved to the Zed `v1.24.2` stable tag.
  The decision, the API churn and the build-time and size deltas are
  recorded in [ADR 0003](../adr/0003-gpui-zed-stable-pin.md).
- **October 2026 (ADR 0006):** moved off the stable tag to the head of Zed
  PR #62379 (`huacnlee/zed@840e095`) for the macOS web renderer; see
  "Deviation From ADR 0003" above.
