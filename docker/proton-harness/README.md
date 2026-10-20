# Proton Harness

A test double for the Proton Mail API, used by the live Proton tests. It is
go-proton-api's fake server (`server/cmd/server`) at the commit pinned in the
`Dockerfile` (`GO_PROTON_API_COMMIT`), with one small patch. It is built and
run only for tests; nothing from it ships in zmail.

Once started it serves:

- the Proton API on `https://127.0.0.1:18443`, over TLS with a certificate
  signed by a throwaway CA generated on every `up`
- go-proton-api's gRPC control service on `127.0.0.1:18080`, which creates
  users, addresses and labels
- a seeded account: `zmail` / `zmail-proton-password` (single-password mode),
  with the addresses `zmail@proton.local` and `zmail.alias@proton.local`, the
  folders `Work` and `Receipts` and the labels `Follow up` and `Family`

Commands:

```bash
scripts/proton-harness.sh up      # build, start, wait for readiness, seed
scripts/proton-harness.sh env     # PROTON_HARNESS_* exports for the running harness
scripts/proton-harness.sh logs
scripts/proton-harness.sh down
```

`up` writes its state to `target/proton-harness/` (or
`$PROTON_HARNESS_STATE_DIR`): `ca.pem`, `state.json` (base URL, CA path,
account, address IDs and label IDs) and `env`. `env` exports
`PROTON_HARNESS_URL` and `PROTON_HARNESS_CA_PEM`, plus the account's username
and password. In Rust, `zmail_test_support::ProtonHarness::start()` runs the
script and returns the same data.

```bash
cargo test -p zmail-test-support --test live_proton_harness -- --ignored --nocapture
```

`PROTON_HARNESS_MODE=native` builds the same commit and patch with a local Go
toolchain (go-proton-api asks for Go 1.26) and runs it without Docker. It's
handy where Docker is missing or cannot unpack images.

## Trusting the harness

The client must trust the harness CA, and only in tests. `zmail-proton-api`
offers that only behind its `test-harness` cargo feature, which only
`zmail-test-support` enables. `ClientConfig::for_test_harness(base_url,
ca_pem)` trusts that CA alone, without the built-in roots. It skips the
production SPKI pins and accepts only an `https://` loopback base URL. The
library reads no environment variable. The feature refuses to compile in a
release-profile build, so production builds keep the fixed API host and its
pins.

## The patch

`api-port.patch` changes only `server/cmd/server/main.go`, in two ways:

- **`--api-port`**: upstream binds the API to `127.0.0.1` on a random port
  (`net/http/httptest`). That is unreachable through a Docker port mapping and
  unknowable without asking the gRPC service. With `--api-port` the API
  listens on that port on all interfaces inside the container, and the script
  publishes it on `127.0.0.1:18443` only.
- **`--tls-cert` / `--tls-key`**: upstream serves TLS with Go's built-in
  `httptest` certificate. That certificate is a CA certificate
  (`basicConstraints CA:TRUE`), and rustls (webpki) refuses a CA certificate
  as a server's end-entity certificate (`CaUsedAsEndEntity`). The patch
  terminates TLS in a `tls.Listener` with the end-entity certificate the
  script makes for `127.0.0.1` and `localhost`. The fake server itself then
  runs in plain mode behind that listener.

With neither flag the server behaves as upstream does. When the pin moves,
check that the patch still applies (`git apply --check`). The image build
and native mode both fail loudly if it doesn't.

## Licence

go-proton-api is MIT licensed (Copyright (c) 2020 James Houlahan, Copyright
(c) 2022 Proton AG). The patch is under the same licence, and the use is
recorded in `about.toml` even though it is test-only.
