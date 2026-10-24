# ADR 0004: Native Proton Mail Support

## Status

Accepted on October 20, 2026 (issue #237). The product decisions below
are Rob's, taken on October 20, 2026; the crypto choice follows the
assessment in this ADR, as he asked.

## Context

Proton Mail stores mail end-to-end encrypted with OpenPGP and does not
offer IMAP, SMTP or JMAP to clients. Today a Zmail user can only reach a
Proton mailbox through Proton Mail Bridge, a separate desktop program
that decrypts mail locally and serves it over IMAP and SMTP on
`127.0.0.1`. Issue #237 asks for native support.

The options were:

- **A. Keep requiring Bridge.** Nothing to build, but users run a second
  program (with its own login and keychain entries), and Zmail cannot
  show Proton's own state (labels, encryption status, address keys).
- **B. Native client.** Talk to Proton's API directly, porting the parts
  of Bridge (`proton-bridge`, GPL-3.0-or-later) and its API library
  (`go-proton-api`, MIT) that Zmail needs.
- **C. Build on an existing third-party client** such as `hydroxide`
  (MIT, Go, being moved to Codeberg) or one of the Rust experiments
  (`proton-api-rs`, unmaintained since 2023). These lag Proton's API and
  track nobody's release cadence.

Proton's API is private: it is documented only by Proton's own open
source clients, and it changes when Proton changes those clients.
Bridge is the client Proton itself ships for "a third-party mail client
on the desktop", so it is the closest reference for traffic that Proton
expects from that kind of program.

## Decision

Zmail implements Proton natively (**option B**), mirroring
[Proton Mail Bridge v3.27.1](https://github.com/ProtonMail/proton-bridge/releases/tag/v3.27.1)
(`43b9237`) and the `go-proton-api` revision it pins (`8a62023`) closely
enough that upstream changes can be tracked file by file:

- **Upstream tracking.** The pins live in
  [`docs/upstream/proton.md`](../upstream/proton.md). Every ported file is
  listed in [`docs/upstream/proton-ports.toml`](../upstream/proton-ports.toml)
  with the upstream path it came from, and a weekly workflow opens an
  issue when one of those upstream paths changes after the pin. Each
  ported file starts with a header naming its upstream path and pin, the
  upstream copyright and licence, and what Zmail changed.
- **Client identity: Bridge's app version, Zmail's User-Agent.** Every
  request sends `x-pm-appversion: <os>-bridge@<Bridge pin>` (today
  `macos-bridge@3.27.1`, `windows-…` or `linux-…`, the platform as
  Bridge's `getAPIOS()` names it) and `User-Agent: zmail/<version> (<os>)`.
  The plan was an honest `zmail@<version>`, but Proton's sign-in only
  accepts Proton's own clients:
  - `zmail@0.16.0` was refused with code 2064 (no platform).
  - `<os>-zmail@<version>` is refused with 2064, "Product `zmail` is not
    valid".
  - `<os>-other@<version>`, Proton's generic product, passes the format
    check but sign-in refuses it with 5002, "Invalid app version"
    (0.16.1).

  On 2026-10-22 Rob decided to send Bridge's app version, since Zmail
  ports Bridge's API behaviour, and to ask Proton for permission and a
  registered identity at the same time. The User-Agent still names Zmail,
  as Bridge itself sends the name of the IMAP client connected to it.
  `BRIDGE_VERSION` in `zmail-proton-api` follows the Bridge pin (a test
  checks it), so the app version moves with the code Zmail ports. The
  sign-in screen tells users that Zmail signs in as Bridge and appears as
  Bridge in their Proton sessions. If Proton grants Zmail its own
  identity, Zmail switches to it. If Proton refuses the header (API codes
  2064, 5001, 5002 or 5003), Zmail reports that to the user and stops;
  trying other identities is Rob's decision, not a fallback.
- **Paid plans only is acceptable.** Proton answers some free accounts
  with code 10004 ("paid plan required") for non-web clients, as it does
  for Bridge. Zmail surfaces that as its own error and does not try to
  work around it.
- **Terms of service.** Proton's terms (last modified October 9, 2026)
  allow automated access "provided that the resulting traffic remains
  indistinguishable from the standard client behavior of human users"
  (clause 2.10 and the paragraph after it), and reserve the right to
  block anything that deviates from normal usage or tries to bypass
  security controls. Zmail therefore follows Bridge's request patterns:
  the same endpoints and page sizes, the event poll interval (20 s plus
  0 to 20 s of jitter), Retry-After on 429 and 503, one token refresh
  on 401, no parallel logins, and no attempt to bypass human
  verification or captchas.
- **TLS pinning.** Connections to Proton's API need both a normal
  WebPKI chain to a trusted root and a leaf certificate whose SPKI
  SHA-256 is one of Bridge's pins
  (`internal/dialer/dialer_pinning.go`). The pins are data
  (`crates/zmail-proton-api/src/pins.txt`), kept in Bridge's
  `pin-sha256="..."` format so the upstream check can compare them.
  Bridge's other two dialer features are not ported: alternative
  routing through DoH-discovered proxies (which skips chain verification
  and relies on the pins alone), and reporting pin mismatches to
  `reports.proton.me` (a privacy leak Zmail does not need).
- **No base-URL override in production.** The API host
  (`https://mail-api.proton.me`) and the pins are constants. The fake
  server test harness uses a `test-harness` cargo feature, off by
  default and enabled only by `zmail-test-support`, which accepts only
  an `https://` URL on a loopback host, trusts only the harness CA and
  skips pinning. The feature refuses to compile in release builds, and
  the library reads no environment variables.
- **Encrypted local store first.** Proton mail is end-to-end encrypted
  on Proton's side; writing it decrypted into Zmail's SQLite store would
  make Zmail the weakest link. Encryption at rest (#139) must ship
  before any Proton code path is reachable from the UI. Until then the
  Proton crates are built and tested but not wired into account
  onboarding or sync dispatch.
- **Scope of the first version.** TOTP two-factor only (no FIDO2),
  combined address mode first, and no code read from the AGPL
  `ProtonMail/clients` monorepo or from the AGPL Rust parts of Proton's
  mail apps.

### Sync: newest mail first, older mail in the background

Bridge lists every message's metadata before its IMAP clients see any
mail. On a large mailbox that is thousands of requests, and a failure
part-way through loses them all. Zmail keeps Bridge's listing (newest
first, 128 messages a page, each page continuing from the last message
ID of the one before, from `stage_metadata.go`) but splits it up
(October 23, 2026, from Rob's testing of 0.17.0 with a large mailbox):

- **First sync.** Take the newest event ID, the user, addresses and
  labels as before, then list only the newest 4 pages (512 messages) and
  write them with every label's mailbox, so the account is usable within
  seconds. A page that comes back with nothing new means the history is
  complete.
- **Checkpoint.** The account's checkpoint is JSON:
  `{"event_id": "...", "backfill_end_id": "<oldest message listed>",
  "listed": 512, "total": 85000}`. `backfill_end_id` is absent once the
  history is complete; `listed` and `total` (Proton's message count when
  the first sync started) only word the progress. A bare event ID, as
  0.17.0 and earlier stored, reads as "events from this ID, history
  complete", so existing accounts carry on without a resync.
- **Backfill.** Every sync first applies the events as before, then,
  while there is a cursor, lists up to 8 older pages (1,024 messages)
  from it, places them with the same label-to-mailbox code and writes them
  with `LocalStore::merge_account_snapshot`. That write only adds: it
  creates missing mailboxes but never removes one, adds threads but never
  prunes them, and adds a conversation's older messages to the thread that
  already holds its newer ones, working out the thread's counts, latest
  time, participants, subject and preview over all of them. Queued local
  read and flag state, hydrated bodies and attachments survive, as with
  the full write. The checkpoint is stored after each write, so a failure
  resumes from the cursor and earlier stretches are kept. While older mail
  is still to come, the sync service starts the next sync 1.5 s later
  (and each event poll starts one, so a backfill stopped by a failure
  carries on); write-back, sending and events run between the stretches.
  Older mail never raises new-mail notifications. The status reads
  "Fetched 2,048 of 85,000 messages; older mail is still arriving".
- **Events.** Each event batch becomes a delta of just the messages it
  names (`MailDelta`): every created or changed message placed by its
  latest labels, every deleted one placed nowhere.
  `LocalStore::apply_mail_delta` takes each message out of the mailboxes
  and conversations it no longer belongs to (removing a thread left
  empty, and working out the others' counts, latest time, subject and
  participants again), then merges the placed messages like a backfill
  stretch. A moved message keeps its downloaded body and attachments.
  Only the touched threads and mailboxes are written, in one
  transaction, so new mail costs the same however large the mailbox is;
  the stored mirror is not read at all. New-mail notifications come from
  the messages the events created that the mirror did not hold yet. An
  update to a message the backfill has not reached places it at once
  without notifying, and listing it later finds it already there. An
  event batch with no message changes writes nothing.
- **Resyncs.** A refresh, label, address or user event still resyncs the
  account, and the resync is paged too: the newest pages replace the
  mirror, which drops the older mail fetched so far (and its downloaded
  bodies), and the backfill starts again from the top.

This sends fewer requests per second than Bridge's sync, with the same
endpoints and page size. Two steps of a write still grow with the
account: rebuilding the address-autocomplete history after mail changed
(one grouped query over the account's messages), and recounting the
threads of each touched mailbox (an indexed count per mailbox).

### Crypto: `proton-srp` for SRP, a port onto `pgp` for keys

Two paths were assessed for SRP login and key unlocking.

**Option 1: Proton's Rust crates.** `proton-srp` 0.8.4 for SRP plus
`proton-crypto-account` 0.20.1 (salts, user and address key unlock) on
`proton-crypto`'s pure-Rust `rustpgp` backend (`proton-rpgp` 0.9.0 over
`pgp` 0.21). The Go backend (`gopgp`, the crates' default) is out of
the question: it builds GopenPGP through cgo.

**Option 2: a direct port onto `pgp` (rPGP).** Port go-proton-api's
`salt.go`, `keyring.go` and `unlock.go` (and Bridge's use of them) onto
`pgp` 0.21, the OpenPGP crate ADR 0002 (#92) already proposes for
Zmail's own OpenPGP support. SRP would be ported from `go-srp`, or taken
from `proton-srp`.

| | Option 1: `proton-srp` + `proton-crypto-account` (`rustpgp`) | Option 2: port onto `pgp` |
| --- | --- | --- |
| Licence metadata | MIT, but every crate publishes only `license-file`, so cargo-deny needs a `[[licenses.clarify]]` entry for each of the four Proton crates | `pgp` is `MIT OR Apache-2.0`; nothing to clarify (one entry if `proton-srp` is used for SRP) |
| New crates in `Cargo.lock` | 99 new, 4 new versions of existing crates. It includes pre-1.0 post-quantum crates that `proton-rpgp` enables unconditionally (`ml-kem`, `ml-dsa` 0.0.4, `slh-dsa` 0.0.3, `kem` pinned to `=0.3.0-pre.0`), plus `serde_with` and a second `darling` | 85 new with `proton-srp` (one of them a second `num-bigint-dig` version), almost all RustCrypto primitives that rPGP needs anyway |
| Maintenance | Proton AG, released on crates.io (latest October 8, 2026); the GitHub repository is a read-only mirror that accepts no contributions, and its README says it is "not intended or vetted for general usage outside Proton". `rustpgp` is the non-default backend | rPGP: active community project (Delta Chat, Proton itself), weekly commits, a security policy and a published security status |
| Review | No published audit of the Proton crates; they inherit rPGP's | rPGP and RustCrypto RSA: audits in 2019 (Open Tech Fund) and December 2024 (Radically Open Security for NLnet), plus the 2024 ETH analysis; all findings fixed |
| API fit with Bridge's flow | Very good: `KeySalt::salted_key_passphrase`, `UserKeys::unlock` and `AddressKeys::unlock` match Bridge's steps | The logic is small (bcrypt the key password with each key salt, unlock user keys, decrypt each address key's token with the user keyring), and a port maps 1:1 onto the Go files the upstream check tracks |
| `RUSTSEC-2023-0071` (`rsa`) | Present (through `pgp` and directly in `proton-rpgp`) | Present (`rsa` is a hard dependency of `pgp`) |

SRP is the one place where local code would mean hand-written modular
arithmetic on secrets. `proton-srp` is small, pure Rust, uses
`crypto-bigint`'s constant-time arithmetic and ships the test vectors
Proton's clients are tested against. Its parameter checks are lighter
than `go-srp`'s: it checks the modulus size, that the modulus is 3 mod 8
and that 2^(N-1) is 1 mod N, and rejects a server ephemeral that is 0
mod N, but it skips `go-srp`'s safe-prime test. That's acceptable
because the modulus must first carry the signature of Proton's
hard-coded modulus key, which Zmail checks before SRP starts.

**Decision: a hybrid.** SRP uses `proton-srp` 0.8.4 with its default
`pgpinternal` feature turned off. The modulus signature is checked
locally against `pgp`, as `go-srp`'s `readClearSignedMessage` does, so
`pgp` keeps its default features off and the `bzip2` crates stay out.
Salts, user keys, address keys and two-password mode are a direct port
of go-proton-api onto `pgp` 0.21.0 with default features off. This
leans on the community-reviewed crate (rPGP) for everything except SRP,
keeps the vendor dependency to one small crate with one cargo-deny
`clarify` entry, avoids the pre-release post-quantum crates, and keeps
the ported files tracked against go-proton-api.

ADR 0002 (#92) and this ADR therefore share the `pgp` crate: Proton
message decryption and Zmail's own OpenPGP support use the same
implementation, the same advisory exceptions and the same audit
history.

**Forks for `RUSTSEC-2026-0387` (October 24, 2026).** `pgp` 0.21.0,
the newest release, pulls `crypto-bigint` 0.5.5 (through `cx448` and the
`elliptic-curve` 0.13 curves), which `RUSTSEC-2026-0387` covers: timing
side channels in subtraction, division and Montgomery multiplication,
fixed only in 0.7.6. Rob decided against ignoring it. Zmail instead
takes `pgp` from a fork pinned to the head of rpgp/rpgp#573, which moves
rPGP to the RustCrypto 0.14 stack (`crypto-bigint` 0.7.6, `rsa`
0.10.0-rc.19, `curve25519-dalek` 5), until a crates.io release contains
it. The key port is unchanged apart from handing rPGP a `rand` 0.10 RNG.
`proton-srp` 0.8.4, also the newest release, depends on `crypto-bigint`
0.5.5 for SRP itself, and Proton's repository takes no contributions, so
Zmail takes it from a fork too: 0.8.4 with one commit that ports it to
`crypto-bigint` 0.7.6 and `rand` 0.10. The port keeps to constant-time
arithmetic on secrets; only the rejection sampler that draws the
ephemeral secrets varies in time, and then only with the public modulus
and the rejected draws, as before. The go-srp and Python SRP vectors pass
unchanged. `docs/upstream/proton.md` records both revisions and how to
move back.

**Zmail-specific key check.** go-crypto validates private key material
against the public key when it decrypts a key (gopenpgp's `Key.Check` is
a no-op for that reason); rPGP does not. Proton's server stores the
locked keys, so a malicious or compromised server could swap in other
private material for a key's public key ("key overwriting", the KO
attacks on OpenPGP) and learn key material from what the client then
produces. After unlocking, Zmail therefore signs a probe with every key
that can sign and verifies it with the public key, and decrypts a probe
encrypted to every key that can encrypt. A key that fails is treated as
not unlocked. This check has no upstream counterpart in Bridge; it is
there because the crypto library changed.

If Proton accounts with post-quantum keys need support later, `pgp`'s
`pqc` feature adds them. Messages compressed with bzip2 by external
OpenPGP senders need `pgp`'s `bzip2` feature, which the hydration phase
turns back on if it needs it.

### `RUSTSEC-2023-0071` (Marvin attack on `rsa`)

The advisory covers RustCrypto `rsa`'s PKCS#1 v1.5 decryption, which can
leak information through timing and through whether decryption succeeds.
An attacker who can submit hundreds of thousands of adaptively chosen
ciphertexts, and learn for each one whether it decrypted (or how long it
took), can decrypt a chosen ciphertext or forge a signature. As of
October 2026 there is no fix: `patched = []` in the advisory,
RustCrypto/RSA#626 (padding not constant-time), #680 (implicit
rejection) and #702 (blinding by default) are open, and `rsa` 0.10 is
still a release candidate (0.10.0-rc.19). The move to `crypto-bigint`
closed the modular exponentiation timing but not the padding oracle.
rPGP lists the issue in its security status, and its RSA decryption
calls `rsa`'s unblinded `decrypt`. The rPGP fork above moved Zmail to
`rsa` 0.10.0-rc.19, which is still affected and still decrypts
unblinded, so the ignore below stays.

How Zmail uses RSA: Proton accounts created before Curve25519 became
the default (2019 to 2021) still have RSA user and address keys. Zmail
decrypts with them locally: address-key tokens during unlock, and the
session key (PKESK) of every message and attachment encrypted to such a
key. Curve25519 (ECDH) keys don't touch `rsa` at all. Signing outgoing
mail with an RSA key is also a local private-key operation.

Assessment: **the attack doesn't reach Zmail's use.** Marvin needs a
decryption oracle, and Zmail exposes none. Decryption happens on the
user's machine and its result never goes back to whoever wrote the
ciphertext. A sender can put crafted messages in a mailbox, but doesn't
learn whether or how fast each one decrypted. RustSec's own workaround
reads: "Local use on a non-compromised computer is fine." Timing
observed by a local attacker is outside Zmail's threat model, since a
compromised machine already exposes the unlocked keys.

The only party that could observe Zmail's behaviour per message is
Proton's server, through the cadence of Zmail's requests. Proton's
end-to-end model doesn't trust the server with message content, so the
hydration phase must follow these design rules:

1. Decryption is decoupled from network scheduling. Message and
   attachment bodies are downloaded and stored first, then decrypted by
   a separate local task, so the timing and order of the next request
   doesn't depend on how long, or whether, a decryption took.
2. Decryption failures are never reported back to Proton or to anyone
   else: no error telemetry, no automatic replies, no read receipts and
   no label changes caused by a failure.
3. Nothing is re-fetched because decryption failed. A message that
   doesn't decrypt is shown as undecryptable and stays that way until the
   user acts.

Sending keeps to the same line. Bridge decrypts the key packets the
server returns for each uploaded attachment to get its session key back,
and only then sends; with an RSA key that would be a decryption of
server-supplied data whose outcome the server sees. Zmail keeps the
session keys it made when it encrypted the attachments and never decrypts
anything during a send.

With those rules in place, `RUSTSEC-2023-0071` is ignored in `deny.toml`
and in the `cargo audit` step of CI, with a reason pointing here. Users
with RSA keys can also migrate to Curve25519 keys in Proton's settings.
Writing Zmail's own RSA code instead would be riskier than the residual
risk above. **Revisit the ignore when `rsa` 0.10 (or a 0.9 release) ships
a fix for #626/#680, or if Zmail ever exposes decryption results to a
network peer.**

## Risks

- **Terms of service and account blocking.** Proton may block or
  restrict accounts whose traffic it sees as abnormal, and its terms
  exclude support or recourse for those restrictions. Zmail's request
  pattern follows Bridge's, but it sends Bridge's app version, so Proton
  can refuse or block it at any time and may treat Bridge's identity from
  another client as a breach of its terms. If that happens Zmail stops
  and the decision goes back to Rob. The fake server doesn't check the
  header, so only a live sign-in shows what Proton accepts; 0.16.0 and
  0.16.1 found the rules above.
- **Private API.** Endpoints, fields and error codes can change without
  notice. Mirroring Bridge's pinned revisions, plus the weekly upstream
  check, is how Zmail notices; a Proton-side change that lands before
  Bridge ships can still break Zmail for a while.
- **Paid plans only.** Free accounts may get 10004 and cannot use Zmail
  natively; they can still use Proton's own web and mobile apps.
- **Human verification and captchas.** Proton can ask for human
  verification (9001) at login. Zmail returns the verification URL and
  token for the user to complete in a browser, then retries with the
  token; it never solves or bypasses the check itself.
- **Pinned keys rotate.** If Proton rotates its API keys beyond Bridge's
  current, hot and cold backups before Zmail picks up new pins, Zmail
  cannot connect until a release ships the new pins. The weekly upstream
  check reports pin changes in `dialer_pinning.go`.
- **Licensing.** Code ported from Bridge is GPL-3.0-or-later and is used
  under GPL-3.0-only, which matches Zmail's licence. Code ported from
  go-proton-api and go-srp is MIT, and the notices are kept in each file,
  in `about.toml` and in `crates/zmail-proton-api/LICENSE-MIT`.
- **Unencrypted local copies.** Until #139 ships, nothing Proton-related
  is reachable from the UI.

## Consequences

- New crates: `zmail-proton-api` (the go-proton-api port: client, auth,
  keys and later messages, labels, events and send) and later
  `zmail-proton` (the Bridge services port: login, sync, message
  building, events and sending). New `zmail-security` secret slots hold
  the Proton session UID, the refresh token and the salted key password;
  the account password itself is never stored.
- The phases are tracked in #237: ADR and pins, the fake-server harness,
  the client core, auth, keys, read sync, hydration, events, pending
  operations, onboarding (hidden until #139), sending, and the weekly
  upstream check.
- `deny.toml` gains one `clarify` entry (`proton-srp`) and one advisory
  ignore (`RUSTSEC-2023-0071`) when those crates arrive with the auth
  and keys work.
