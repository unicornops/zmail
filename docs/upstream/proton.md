# Proton Upstream Record

Zmail's native Proton Mail support (issue #237,
[ADR 0004](../adr/0004-proton-native.md)) ports code from Proton Mail
Bridge and the Go libraries it uses, and follows their behaviour closely
so Proton sees the same traffic pattern as from Bridge. This file records
which upstream revisions the ported code matches.

## Current Pin

<!-- proton-pin:start -->
| Repository | Release | Commit |
| --- | --- | --- |
| [ProtonMail/proton-bridge](https://github.com/ProtonMail/proton-bridge) | `v3.27.1` | `43b9237786204a0215abfd4dcc51ee7bb92f52d4` |
| [ProtonMail/go-proton-api](https://github.com/ProtonMail/go-proton-api) | Bridge v3.27.1 go.mod | `8a62023d3e38cb29749bcfc01bf44eafda6cec82` |
| [ProtonMail/go-srp](https://github.com/ProtonMail/go-srp) | `v0.0.7` (Bridge v3.27.1 go.mod) | `167a266a601fa1875838578be2edfbd4e97e6358` |
| [ProtonMail/gluon](https://github.com/ProtonMail/gluon) | Bridge v3.27.1 go.mod | `7e800978ab4a9942829eb6116e33ea0278c03e91` |
| [ProtonMail/gopenpgp](https://github.com/ProtonMail/gopenpgp) | `v2.10.0-proton` (Bridge v3.27.1 go.mod) | `e66eaf2252f21870add7f3dd6d782572c02ce2a8` |
<!-- proton-pin:end -->

Bridge's `go.mod` pins go-proton-api as
`v0.4.1-0.20260901104215-8a62023d3e38` and gluon as
`v0.17.1-0.20260901104218-7e800978ab4a`; the commits above are those
pseudo-versions resolved to full hashes. go-proton-api and gluon have no
release tags at those commits.

Rust crates that implement Proton protocols are pinned by `Cargo.lock`
like any other dependency, not here:

- `proton-srp` (crates.io, from
  [ProtonMail/proton-crypto-rs](https://github.com/ProtonMail/proton-crypto-rs)):
  SRP proofs and the bcrypt password hashes, with the default
  `pgpinternal` feature off.
- `pgp` (rPGP): OpenPGP, shared with ADR 0002. Taken from a fork for
  now; see below.

## rPGP Fork

`pgp` is a git dependency on
[lazzurs/rpgp](https://github.com/lazzurs/rpgp) at
`696fe2f437e401f21cac8a1f32f9144a4b95d951` (branch
`zmail/rustcrypto-0.14`), the head of upstream pull request
[rpgp/rpgp#573](https://github.com/rpgp/rpgp/pull/573) "Adopt
RustCrypto new releases". The fork holds that commit unchanged; Zmail
carries no patches of its own.

Why: `pgp` 0.21.0, the newest release, depends on `crypto-bigint` 0.5.5
(through `cx448` and the `elliptic-curve` 0.13 curves), and
[RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387)
covers every `crypto-bigint` before 0.7.6. #573 moves rPGP to the
RustCrypto 0.14 stack (`crypto-bigint` 0.7.6, `elliptic-curve` 0.14,
`rsa` 0.10.0-rc.19, `curve25519-dalek` 5, `rand` 0.10). Its own
`[patch.crates-io]` for `rsa` does not apply to Zmail, which builds with
crates.io `rsa` 0.10.0-rc.19. `deny.toml` allows the fork's git source.

Zmail hands rPGP a `rand` 0.10 RNG through the `rand_pgp` workspace
dependency; everything else keeps `rand` 0.8, which `proton-srp` uses.

To move back once a `pgp` release on crates.io contains #573:

1. Set `pgp = { version = "<release>", default-features = false }` in
   the root `Cargo.toml` and run `cargo update -p pgp`.
2. Remove `https://github.com/lazzurs/rpgp` from `allow-git` in
   `deny.toml`.
3. Keep `rand_pgp` while that release takes `rand` 0.10; drop it if it
   matches the workspace `rand` again.
4. Run the workspace tests (the key, unlock, sign, encrypt and send
   package tests in `zmail-proton-api`) and the Proton harness, then
   remove this section and update ADR 0004's crypto section.

## Licences

| Repository | Licence | How Zmail uses it |
| --- | --- | --- |
| proton-bridge | GPL-3.0-or-later | Ported under GPL-3.0-only (Zmail's licence), keeping "Copyright (c) Proton AG" and a dated modification notice in each file |
| go-proton-api | MIT | Ported; MIT notice in each file and in `crates/zmail-proton-api/LICENSE-MIT` |
| go-srp | MIT | Test vectors and the modulus-signature check; MIT notice kept |
| gluon | MIT | Reference only so far (Bridge's IMAP layer); nothing ported |
| gopenpgp | MIT | Reference for key-unlock semantics; nothing ported |
| proton-crypto-rs | MIT | `proton-srp` as a dependency; its manifest only names a licence file, so `deny.toml` clarifies it as MIT |

`about.toml` lists every ported source file with its upstream and
licence, because cargo-about only lists crates.

## Ported Files

[`proton-ports.toml`](proton-ports.toml) maps each ported Zmail file to
the upstream file (or directory, with a trailing `/`) it follows:

```toml
[[port]]
repo = "go-proton-api"        # a repository name from the pin table
upstream = "manager.go"       # path in that repository at the pinned commit
local = "crates/zmail-proton-api/src/manager.rs"
```

Every Rust file ported from upstream also starts with a header naming the
upstream path and pinned commit, the upstream copyright and licence, and
what Zmail changed. Keep the header and the manifest entry in step when
porting more code.

## Upstream Check

A weekly workflow compares the pinned commits with upstream's newest
Bridge release and the go-proton-api revision it uses. When any upstream
path in `proton-ports.toml` changed after its pin, it opens (or
updates) an issue listing the changed files with links to the upstream
diffs. Someone then ports the changes and moves the pin table above in
the same pull request.

Things that need extra attention on every bump:

- **TLS pins.** `internal/dialer/dialer_pinning.go` in Bridge holds the
  API's SPKI pins; `crates/zmail-proton-api/src/pins.txt` must hold the
  same `pin-sha256` lines.
- **Error codes and retry rules** in go-proton-api's `response.go` and
  `manager_builder.go`.
- **The modulus signing key** in go-srp's `srp.go`, if it ever rotates.

## History

- **October 20, 2026 (issue #237):** first pin, Bridge `v3.27.1`.
- **October 24, 2026:** `pgp` moved to the rPGP fork above
  (RUSTSEC-2026-0387).
