# Upstream: Git-Pinned Dependencies

Four crates come from git forks pinned by commit (`rev`) in the root
`Cargo.toml`, not from crates.io releases. Dependabot skips them
(`.github/dependabot.yml`), `deny.toml` allows their git sources, and
advisory databases cannot match a git source, so an upstream fix
(including a security fix in OpenPGP or GPUI code) would not reach Zmail
on its own. The weekly `git-pins` workflow watches them instead (issue
#472).

## The Pins

| Crates | Fork | Follows | The fork can be dropped when | Details |
| --- | --- | --- | --- | --- |
| `gpui`, `gpui_platform` | [`huacnlee/zed`](https://github.com/huacnlee/zed) | [zed-industries/zed#62379](https://github.com/zed-industries/zed/pull/62379) (native window composition) | A Zed stable release contains the merged pull request, or whatever replaces it. The `zed-upstream` workflow then moves both lines back to that release tag; GPUI stays a git dependency, on a Zed tag, as ADR 0003 says. | [zed.md](./zed.md), "Deviation From ADR 0003" |
| `pgp` | [`lazzurs/rpgp`](https://github.com/lazzurs/rpgp) | [rpgp/rpgp#573](https://github.com/rpgp/rpgp/pull/573) (RustCrypto 0.14, `crypto-bigint` 0.7.6) | A crates.io `pgp` release contains #573, so `pgp` no longer pulls the `crypto-bigint` 0.5.5 of RUSTSEC-2026-0387. | [proton.md](./proton.md), "rPGP" |
| `proton-srp` | [`lazzurs/proton-crypto-rs`](https://github.com/lazzurs/proton-crypto-rs) | [ProtonMail/proton-crypto-rs](https://github.com/ProtonMail/proton-crypto-rs) releases (a read-only mirror: no pull request to follow) | A crates.io `proton-srp` release depends on `crypto-bigint` 0.7.6 or later instead of 0.5. | [proton.md](./proton.md), "proton-srp" |

The `[patch.crates-io]` git entries (`async-task`, `calloop`) are not forks
of Zmail's: they mirror Zed's own patches at the pinned GPUI, and
`scripts/bump-zed-gpui.sh` keeps them in step.

## The Weekly Check

`.github/workflows/git-pins.yml` runs every Monday and on demand
(`workflow_dispatch`). It runs `scripts/check-git-pins.py --update-issue`,
which reads each pin's fork, commit and crate versions from `Cargo.toml`
and `Cargo.lock`, and what the fork follows from
[`git-pins.toml`](./git-pins.toml), then reports for each pin:

- the pinned commit and the versions its crates declare;
- the upstream pull request it follows: open, merged or closed; its head,
  and whether that is still the pinned commit; once merged, whether the
  latest release contains the merge commit;
- the latest upstream release: Zed's latest GitHub release for GPUI, the
  newest non-yanked stable crates.io version for `pgp` and `proton-srp`
  (with the `crypto-bigint` requirement of that `proton-srp` release);
- every RustSec advisory filed for the crate names, and whether the version
  the fork declares is in its affected range.

The report is the body of one open issue, found by the
`<!-- git-pins-check -->` marker in its body. The first run creates it; later
runs edit it only when its title or body changed, so a quiet week sends no
notification. Its title counts the findings (`Git pins: 3 findings`, or
`Git pins: nothing to act on`); the issue stays open while any git pin
remains. The workflow's token can read the repository and write issues,
nothing else, and it never opens pull requests or changes the repository.

RustSec advisories come from a shallow clone of
[rustsec/advisory-db](https://github.com/rustsec/advisory-db), the same
database `cargo audit` and `cargo deny` read. Running `cargo audit` itself
would not help: it matches only crates.io packages, which is the gap this
check fills. The clone costs no GitHub API quota, and the advisories carry
the patched and unaffected version ranges, which the script compares with
the version in the fork's `Cargo.toml`. A fork can carry a fix that its
version number does not show (the `pgp` fork says 0.21.0 but is not the
0.21.0 release), so an advisory marked as affecting the pin is a prompt to
check, not a verdict. A run costs about three GitHub API calls.

Run it by hand (`gh` must be signed in; nothing is changed):

```bash
scripts/check-git-pins.py              # print the report (--dry-run)
scripts/check-git-pins.py --validate   # offline: Cargo.toml and git-pins.toml agree
scripts/test-check-git-pins.sh         # offline: fixtures in scripts/fixtures/git-pins
```

`GIT_PINS_ADVISORY_DB=~/.cargo/advisory-db` reuses the checkout
`cargo audit` keeps instead of cloning one. CI's quality job runs the
offline test, which includes `--validate`: every git dependency in
`[workspace.dependencies]` must have a `[[pin]]` in `git-pins.toml`, and
every `[[pin]]` must name git dependencies, so a new fork cannot go
unwatched and a dropped one is removed from the file in the same pull
request.

## What To Do With A Finding

- **The pull request was closed without being merged.** The fork no longer
  leads anywhere. Find out why (superseded by another pull request, or
  abandoned), then decide where the pin goes: a replacement pull request
  (change `pull_request` in `git-pins.toml`, and for GPUI the `ZED_PR`
  default in `scripts/bump-zed-gpui.sh`), a fork Zmail maintains, or back to a release with the
  feature removed. For GPUI that is Rob's call (ADR 0006).
- **The head of the pull request moved.** Look at the linked changes. To
  follow them, move the pin by hand (for GPUI: "To follow a new head of the
  pull request by hand" in [zed.md](./zed.md); for `pgp`: fast-forward
  `lazzurs/rpgp` to the new head and change the `rev`), run the workspace
  checks and, for `pgp`, the Proton harness. Staying put is fine while the
  pin works; the finding stays until the pin and the head agree.
- **The pull request was merged.** Once the latest release contains it,
  drop the fork: GPUI by the `zed-upstream` workflow's next bump pull
  request, `pgp` and `proton-srp` by the steps in [proton.md](./proton.md).
  Until then, nothing to do.
- **A newer crates.io release is out.** Read its changes (the report links
  the diff from the pin) for fixes, security fixes above all, that the fork
  lacks, and check whether it meets the drop condition above. Carry a fix
  into the fork, or move back to crates.io.
- **A RustSec advisory affects the pinned version.** Check whether the fork
  carries the fix. If it does not, carry it into the fork (or move to a
  patched release) as a security fix; if it does, note that in the issue.

When a fork is dropped, remove its `[[pin]]` from `git-pins.toml`, its
`allow-git` entry from `deny.toml` and its `ignore` entry from
`.github/dependabot.yml`.

## The GPUI Bump Script And The Pull Request Pin

`scripts/bump-zed-gpui.sh` still works while GPUI is pinned to the fork: it
reads the `rev` pin, asks GitHub about Zed PR `ZED_PR` (default 62379), and
moves to a stable tag only once the pull request is merged and that release
contains its merge commit. Until then the weekly `zed-upstream` run stops
with a notice, or with a warning when the pull request was closed without
being merged, since no release will ever contain it. It never follows a new
head of the pull request; that, and moving off a closed pull request, are
the manual steps above.
