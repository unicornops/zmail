# Fuzzing zmail's mail parsing

Every message zmail syncs goes through mail-parser and, below it, encodify's
SIMD decoders (`unsafe` code, AVX2/SSSE3/SSE2 kernels picked at run time).
These [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets feed them
hostile input under AddressSanitizer.

| Target | What it runs |
| --- | --- |
| `zmail_parse` | `zmail_imap::hydrated_body_from_rfc822` (zmail's real parse, body split, HTML sanitiser, attachment extraction and threading headers), the sync-time header reads it mirrors from zmail-imap (`accumulate_fetch`, `conversation_key`), and a sweep of mail-parser's view API over every part and nested message |
| `encodify_decoders` | encodify's base64, quoted-printable, RFC 2047, UTF-7 and percent decoders and mail-parser's charset and transfer decoding, with checks that `decoded_len` is exact, buffers are never overrun and every string is valid UTF-8 |

The crate is not a workspace member: it needs nightly and has its own
`Cargo.lock`, which the fuzz workflow moves to the app's mail-parser,
encodify and simdutf8 versions when it lags. `corpus/<target>` holds small
minimised seeds made from zmail's own test messages and synthetic MIME (none
of mail-parser's test resources).

## Running locally

```bash
cargo install cargo-fuzz
cd fuzz
# Replay the seeds.
cargo +nightly fuzz run -O -a zmail_parse corpus/zmail_parse -- -runs=0
# Fuzz, keeping new inputs out of the committed corpus.
mkdir -p /tmp/zmail-fuzz
cargo +nightly fuzz run -O -a zmail_parse /tmp/zmail-fuzz corpus/zmail_parse -- \
  -fork=4 -max_len=65536 -timeout=10 -rss_limit_mb=2048 -malloc_limit_mb=512
```

`-a` keeps debug assertions on (mail-parser checks its UTF-8 spans with
`debug_assert!`). On Linux the build needs `pkg-config` and `libdbus-1-dev`
(zmail-security's keyring). The builds go to the repository's `target/`
(`.cargo/config.toml`).

### SIMD tiers

encodify and mail-parser use the best kernels the CPU has, so a machine with
AVX2 fuzzes only those. To fuzz the others, build with encodify's cfgs:

```bash
RUSTFLAGS="--cfg encodify_no_avx2" cargo +nightly fuzz run -O -a encodify_decoders ...  # SSSE3/SSE2
RUSTFLAGS="--cfg encodify_scalar" cargo +nightly fuzz run -O -a encodify_decoders ...   # portable code
RUSTFLAGS="-C target-cpu=native" cargo +nightly fuzz run -O -a zmail_parse ...          # kernels inlined
```

mail-parser has no such cfg: its kernel choice (`scan::Kernel::best`) is
crate-private, so its SSE2 and SSSE3 scanners can only be reached on a CPU
without AVX2, or with a local patch to `Kernel::best`. MemorySanitizer also
works (`-s memory --build-std`, needs the `rust-src` component).

## What has been found

The first runs (about 18 CPU hours on mail-parser 1.0.1 and encodify 1.0.2,
every SIMD tier, ASan and MSan) found nothing in mail-parser or encodify.
Miri found one problem below them, in simdutf8 0.1.5: its AVX2 validator
prefetches through `input.as_ptr().add(idx + 128)`, which points past the end
of the buffer near its end. The pointer is never read, so nothing faults, but
the offset is undefined behaviour. mail-parser reaches it through
`MessagePart::text()` for a message ending in 128 or more bytes of non-ASCII
UTF-8. It is rusticstuff/simdutf8#129, fixed upstream by #120 and not yet
released; move to the release that has it.

To check it under Miri:

```bash
RUSTFLAGS=-Ctarget-feature=+avx2 cargo +nightly miri run --target x86_64-unknown-linux-gnu
```

on a program that parses such a message and calls `text()` on its body.

## In CI

`.github/workflows/fuzz.yml` builds both targets, replays the seeds and
fuzzes each for five minutes. It runs when this directory, zmail-imap,
zmail-core or zmail-security change, or when Cargo.lock moves mail-parser,
encodify or simdutf8, and weekly. A crash, a timeout or an out-of-memory
input fails the job and is uploaded as the `fuzz-artifacts` artefact;
reproduce it with `cargo +nightly fuzz run -O -a <target> <file>`.
