# ADR 0005: Encrypting the Local Store at Rest

## Status

Accepted on October 21, 2026 (issue #139); the choices that were open
are recorded under "Decisions" at the end. A release blocker for native
Proton Mail (#237, ADR 0004): Proton mail is end-to-end encrypted, and
Zmail must not be the place where it sits on disk in plaintext. The
decision applies to every account, not only Proton ones.

## Context

Everything Zmail keeps lives in one data directory:

- `zmail.sqlite3` (plus `-wal` and `-shm`): accounts, identities,
  mailboxes, headers, hydrated bodies (`body_text`, `sanitized_html`),
  drafts and their bodies, the outbox, pending operations, address
  history and the FTS5 search index (`search_documents`).
- `attachments/`: received attachment blobs written by the IMAP and JMAP
  adapters (`incoming-<mailbox>-<uid>-<n>-<filename>`), and copies of
  files attached to drafts (`draft-<id>-<n>-<filename>`). The names carry
  the mailbox and the attachment's filename.
- `opened-attachments/`: plaintext copies handed to the system's default
  app by the reader's Open button, pruned after a day.
- `zmail-app backup` writes `VACUUM INTO` copies of the database.

Account secrets already live in the OS keychain (`zmail-security`,
service `dev.zmail.desktop`); nothing else is encrypted.

The threat model is **a stolen, lost or imaged disk, or a copied data
directory** (a backup, a synced folder, a disposed laptop), read by
someone who does not hold the user's OS login. It is not a compromised
running session: see "What this does not protect".

## Decision

### 1. Database: SQLCipher, through rusqlite

The store is encrypted with [SQLCipher](https://www.zetetic.net/sqlcipher/)
Community Edition, built by `libsqlite3-sys` from its bundled amalgamation
(rusqlite's `bundled-sqlcipher` features). SQLCipher encrypts every page
of the database, the WAL and the rollback journal with AES-256-CBC and
authenticates each page with HMAC-SHA512 (SQLCipher 4 defaults: 4 KiB
pages, a random per-database salt in the first 16 bytes, 80 bytes per
page for the IV and MAC). The schema, the FTS5 index, every index and
every query stay exactly as they are; the only code change is a
`PRAGMA key` as the first statement on each connection.

The key is a raw 256-bit key (`PRAGMA key = "x'…'"`), so SQLCipher skips
its PBKDF2 passphrase stretching: the key is random, not a password, and
connections open often (see Performance).

Alternatives considered:

| Option | Search and indexes | Metadata protected | Code and risk | Verdict |
| --- | --- | --- | --- | --- |
| **SQLCipher** (`bundled-sqlcipher`) | Unchanged: FTS5, B-tree indexes, `ORDER BY`, prefix search all work | Everything in the file, including the schema, the index and row counts | One pragma; mature (since 2008), used by Signal Desktop and Element's Seshat search index; the C is SQLite plus a codec | **Chosen** |
| Per-column or app-level AEAD (encrypt `body_text`, `subject`, …) | Broken: FTS5 has to see plaintext to tokenise, and the index then *is* the plaintext; sorted and indexed lookups on subject or sender stop working; deterministic encryption to keep equality leaks frequencies | Only the chosen columns; the index, senders, dates and sizes leak | Touches most queries in a 10k-line store | Rejected |
| Encrypted SQLite VFS written in Rust | Unchanged | Like SQLCipher | Needs `unsafe` FFI (the workspace forbids `unsafe_code`) or an immature third-party VFS crate, and a home-made page format | Rejected |
| SQLite3 Multiple Ciphers (no OpenSSL; ChaCha20-Poly1305) | Unchanged | Like SQLCipher | Not supported by rusqlite or `libsqlite3-sys`; only single-maintainer `-src` crates, so a forked `-sys` crate | Rejected for now; the fallback if OpenSSL is unacceptable |
| Rely on FileVault, BitLocker or LUKS | Unchanged | Only while the machine is off, and only if the user turned it on | Nothing to build | Recommended to users, not sufficient alone |

**Crypto provider per OS.** SQLCipher calls a crypto library; which one
`libsqlite3-sys` links depends on the feature and the OS:

| OS | Feature | Provider | Extra build and runtime needs |
| --- | --- | --- | --- |
| macOS | `bundled-sqlcipher` | CommonCrypto (Apple's, part of the OS) | None; links `Security` and `CoreFoundation` |
| Windows | `bundled-sqlcipher-vendored-openssl` | OpenSSL 3.6, built from source by `openssl-src` and linked statically | Strawberry Perl (on the runners); NASM, otherwise OpenSSL builds with `no-asm` and loses AES-NI. CI and release install NASM |
| Linux | `bundled-sqlcipher-vendored-openssl` | OpenSSL 3.6, static, as on Windows | A C toolchain and Perl, already on the runners |

`bundled-sqlcipher` without vendoring needs `OPENSSL_DIR` on Windows (the
build panics without it). On Linux it would link the system's
`libcrypto.so.3` instead: no OpenSSL build, distro security updates, but
a runtime dependency (`libssl3` in the `.deb`, and a tarball that fails on
systems without OpenSSL 3). **Linux uses the vendored OpenSSL too.** Rob
asked for the most secure option, and vendoring is it: the OpenSSL
version is pinned in `Cargo.lock` and identical on Linux and Windows, CI's
tests on Linux exercise the same code Windows ships, the tarball stays
self-contained, and RustSec tracks `openssl-src`, so CI's `cargo audit`
and `cargo deny` fail the build when the pinned OpenSSL has a known
vulnerability, instead of trusting whatever `libcrypto` each user's
distribution has. OpenSSL updates come through `openssl-src` releases.
Moving to a Rust-native crypto backend later is #254. The features are set per target in
`crates/zmail-store/Cargo.toml` (`[target.'cfg(target_os = "macos")'.dependencies]`
and its `not` twin); resolver 3 keeps them apart.

**Version.** rusqlite 0.37's `libsqlite3-sys` 0.35 bundles SQLCipher 4.6.1,
which is built on SQLite 3.46.1, while the plain bundle today is SQLite
3.50.2: turning SQLCipher on as-is is a SQLite downgrade (and drops the
fixes for CVE-2025-29087 and CVE-2025-6965; neither is reachable without
attacker-written SQL, which Zmail never runs). rusqlite 0.40.2
(`libsqlite3-sys` 0.38.2) bundles SQLCipher 4.14.0 on SQLite 3.51.3.
**So rusqlite is bumped to 0.40 first, in its own PR (#255)**, then
SQLCipher is turned on. Tried here: with rusqlite's `fallible_uint` feature (0.40
no longer reads `usize` without it) the store compiles unchanged and its
tests pass on SQLCipher 4.14. `Cargo.lock` changes `rusqlite` 0.37.0 →
0.40.2, `libsqlite3-sys` 0.35.0 → 0.38.2 and `hashlink` 0.10.0 → 0.12.2,
and adds `sqlite-wasm-rs` 0.5.5 and `rsqlite-vfs` 0.1.1 (both MIT), which
are only built for `wasm32-unknown-unknown`. The store uses nothing newer
than SQLite 3.38 (`unixepoch`), and plain FTS5.

**Licences and dependency delta** for SQLCipher itself (the same on
rusqlite 0.37 and 0.40):

| Crate | Licence | Builds C | In `Cargo.lock` today |
| --- | --- | --- | --- |
| `libsqlite3-sys` (bundles SQLCipher 4.x: BSD-3-Clause, Zetetic LLC; SQLite: public domain) | MIT | yes (already) | yes |
| `openssl-sys` 0.9.117 | MIT | no (bindings) | **new** (Linux, Windows) |
| `openssl-src` 300.6.1+3.6.3 (OpenSSL 3.6.3: Apache-2.0) | MIT/Apache-2.0 | yes (OpenSSL) | **new** (Linux, Windows) |

`Cargo.lock` gains exactly those two packages (and `openssl-sys` in
`libsqlite3-sys`'s dependency list); `cc`, `pkg-config`, `vcpkg` and
`libc` are already locked. All the licences are in `deny.toml`'s allow
list, so `deny.toml` doesn't change. SQLCipher's BSD-3-Clause notice has
to ship with binaries, and `libsqlite3-sys`'s metadata only says MIT, so
the notice is recorded in `about.toml` (as the ported files are) and
installed next to the Inter licence in the `.deb`.

**Build impact.** Locally (12 threads, shared machine), `openssl-src`'s
build script takes 32 s; SQLCipher compiles in the same 5 s as SQLite.
Expect one to two minutes more per cold build on the 4-core Linux
runners and two to four on Windows (Perl `Configure` plus `nmake`); macOS
is unchanged. CI has no target cache, so every job that compiles the
store pays it once per profile: `quality` (clippy and tests), the three
`Build` jobs, `UI Screenshots` and `Performance budgets`. Windows and
macOS can only be checked by CI.

### 2. Files: AES-256-GCM in chunks, with opaque names

Attachment blobs, draft attachment copies and anything else the store
writes beside the database go through one module,
`zmail_security::vault`, so the IMAP and JMAP adapters stop calling
`fs::write` themselves (they already depend on `zmail-security`; the sync
target carries a `BlobVault` handle instead of a bare directory). Format:

```
magic "ZMB1" | version 1 | key id (16 bytes) | salt (32 bytes)
then 64 KiB chunks: AES-256-GCM, STREAM construction (aead::stream, big-endian 32-bit counter + last-chunk flag)
```

- Each file gets its own key, HKDF-SHA256(blob key, salt), so nonces
  never repeat across files and GCM's per-key limits don't arise. The
  header is the associated data of every chunk; truncation, reordering
  or swapping chunks between files fails authentication.
- Chunking keeps memory flat for large draft attachments (received ones
  are capped at 25 MiB) and lets the open flow stream.
- **Names are opaque**: the file name is HMAC-SHA256(name key, the name
  the adapter would have used), hex. The name stays deterministic, so a
  re-hydrated message overwrites its old blob as it does today, but
  neither the mailbox nor the attachment's filename shows on disk.
  Writes go to a temporary name, are fsynced and renamed into place.

**AES-256-GCM over XChaCha20-Poly1305.** Both are fine with per-file keys.
`aes-gcm` 0.10.3 (with `aes`, `ghash`, `aead`, `hkdf`, `hmac` and `sha2`)
is already in the tree through `pgp`, so the only change is turning on
`aes-gcm`'s `stream` feature, with no new crates; `chacha20poly1305` would
add three (`chacha20poly1305` 0.10.1, a second `chacha20` at 0.9.1, and
`poly1305` 0.8.0). `aes` picks AES-NI or ARMv8 at runtime and falls back
to a constant-time bitsliced implementation. Measured here: 1.35 GB/s
for 25 MiB with AES-256-GCM STREAM, 0.9 to 1.5 GB/s with XChaCha20-Poly1305;
either is far faster than the disk. All RustCrypto crates are MIT OR
Apache-2.0.

**Opening an attachment with another app.** An external app can only
read a plaintext file, so Open still writes one, but for less time and,
where the OS allows, not to disk:

1. The blob is decrypted, streaming, into a fresh owner-only directory
   (0700, file 0600 on Unix; as today) under a random name, never under
   the data directory's predictable path.
2. On Linux the copy goes to `$XDG_RUNTIME_DIR/zmail/opened` when that is
   set: a per-user tmpfs, so the plaintext stays in RAM (or swap) and
   vanishes at logout. macOS and Windows have no per-user RAM file
   system; there it goes to the cache directory, which is on disk.
3. Copies are deleted when Zmail quits, everything left over is deleted
   at the next start (not only copies older than a day, as today), and
   while Zmail runs copies older than one hour are removed. Deleting a
   file an app still has open is usually harmless on macOS and Linux (the
   app keeps its open file, though an app that reopens the path later
   finds it gone); on Windows the delete fails while it is open and
   is retried at the next sweep.
4. Deletion is an unlink, not a secure wipe: on SSDs overwriting is
   meaningless, and the blocks stay readable until TRIM or reuse. On
   macOS and Windows a copy that was opened is therefore exposed to a
   later disk image; that is the honest cost of "open with another app".

Save As and Save All write plaintext where the user chooses: that is an
explicit export, as is `export-account` (mbox).

### 3. Keys

- **One random 256-bit data key per store**, from the OS CSPRNG
  (`OsRng`). Subkeys come from HKDF-SHA256 with distinct labels: the
  SQLCipher key (`zmail/store/sqlcipher/v1`), the blob key
  (`zmail/store/blob/v1`) and the blob-name key (`zmail/store/blob-name/v1`).
- **Kept in the OS keychain** through `zmail-security` (macOS login
  keychain, Windows Credential Manager, Secret Service on Linux), under
  `dev.zmail.desktop` / `store-<key id>`: the keychain is what wraps it,
  with the user's login. Beside the database, `zmail.store-key.json`
  holds only non-secret facts: the format version, the key id and the
  cipher settings. That lets Zmail tell "this store needs key X and the
  keychain has no X" apart from "this is a plaintext store", and leaves
  room for a passphrase-wrapped copy of the key later.
- Creation order is crash-safe: generate, write to the keychain, read it
  back and compare, write the sidecar, and only then write encrypted
  data. Zmail never deletes the keychain entry except when the user
  resets the store.
- In memory the key is held once per process in a `Zeroizing` buffer
  behind an `Arc`, with a `Debug` that prints nothing, and the hex string
  a `PRAGMA key` needs is zeroised after each use. Ephemeral stores (tests,
  UI fixtures, the performance run) get a random in-memory key and are
  encrypted too, so every test exercises the encrypted path.
- **No passphrase by default.** An optional passphrase (Argon2id-wrapped
  data key in the sidecar, asked for at start) is a later feature and
  needs a new dependency; it is also the answer for Linux systems with no
  Secret Service. Not in this stack.
- **No keychain, or a locked one.** If the keychain can't be reached or
  refuses (locked, access denied, no Secret Service on Linux), Zmail shows
  the error with Retry and does not open the store. It never falls back
  to plaintext. On macOS, ad-hoc signed builds change signature with
  every update, so the first launch after an update shows the keychain's
  "allow access" prompt, as it already does for account passwords.

**Losing the key.** If the sidecar names a key the keychain no longer has
(a reinstalled OS, a reset keychain, the data directory copied to another
machine), the store can't be read, by anyone, which is the point. Zmail
then shows a recovery screen instead of the workspace:

- It says what happened, that mail on the servers is safe, and that
  Zmail can start a new store and download it again.
- It says what is lost for good: **drafts, mail waiting in the outbox,
  queued changes not yet sent to the server**, account settings and
  local preferences. The counts can't be shown (they are inside the
  unreadable store), so the warning is unconditional.
- "Start Over" moves the old files aside (`zmail.sqlite3.unreadable-<date>`
  and the blobs with it) rather than deleting them, so a keychain restored
  from a backup can still open them; the user deletes them from Settings,
  or by hand. Accounts then need setting up again (their keychain secrets
  are most likely gone too).
- To lower the chance of that loss, Settings shows where the key lives,
  and the macOS and Windows keychains are covered by the OS's own
  migration and backup (Migration Assistant, Time Machine, roaming
  profiles).

**Rotation.** Two cases, both reusing the migration code below:

- *Rotating the data key* (suspected compromise, or a periodic policy):
  a new key id, the database re-encrypted by copy with `sqlcipher_export`
  into a new file, every blob re-encrypted (the header names its key id,
  so a crash midway leaves a mix that still opens with both keys), then
  the swap, then the old keychain entry deleted. A user who fears their
  old key leaked must also assume old disk images are readable;
  rotation protects what is written afterwards.
- *Changing how the key is wrapped* (adding a passphrase later) does not
  touch the data, because the data key itself doesn't change.

This stack ships the format support (key ids in the sidecar and in every
blob header) and an `zmail-app store rotate-key` command; a Settings
button can follow.

### 4. Migrating existing plaintext stores

Existing installs have a plaintext database and plaintext blobs. At the
first start of a build with this change, before any window or sync
touches the store:

1. **Detect.** A file that starts with `SQLite format 3\0` is plaintext;
   a SQLCipher file starts with its random salt. The header is the source
   of truth, not the sidecar.
2. **Key.** Create the key as in §3, before any encrypted byte exists.
3. **Database, by copy.** Checkpoint the WAL (`wal_checkpoint(TRUNCATE)`),
   `ATTACH` a new `zmail.sqlite3.encrypting` with the key, run
   `SELECT sqlcipher_export('encrypted')`, copy `user_version`, detach,
   then open the new file with the key and check it
   (`PRAGMA cipher_integrity_check`, `quick_check`, and row counts of
   `schema_migrations`, `accounts`, `threads`, `messages`, `drafts` and
   `outbox_jobs` against the source). fsync it, close the source, remove
   its `-wal` and `-shm`, and rename the new file over `zmail.sqlite3`
   (a single rename on the same volume: `rename(2)` on Unix, with the
   directory fsynced, and `MoveFileEx` with replace on Windows).
   In-place encryption isn't offered by SQLCipher for a plaintext file,
   and a copy is what makes it crash-safe.
4. **Blobs, file by file.** For each `storage_path` in
   `message_attachments` and `draft_attachments` that isn't an opaque
   name yet: encrypt the plaintext file to its opaque name (temp, fsync,
   rename), delete the plaintext file, then point every row that used the
   old name at the new one. The new name follows from the old one, so a
   crash between the steps is finished by the next start, which runs this
   step whenever a row still has an old name. Last, any plaintext file left
   in `attachments/` (no row names one any more) is deleted, and so are
   temporary files older than an hour. The open cache is emptied at every
   start anyway (§2).
5. **Crash safety.** A crash before the rename leaves the plaintext
   database untouched; the next start deletes the partial `.encrypting`
   file and starts again. A crash after it leaves an encrypted database
   and some plaintext blobs; the next start sees the encrypted header and
   resumes at step 4, which is idempotent per file. There is never a
   moment where neither a readable plaintext nor a readable encrypted
   copy exists.
6. **Disk space.** The database copy needs free space about the size of
   the database (backups made with `zmail-app backup` are not touched;
   they stay plaintext, and the user is told so once, with their
   location). Blobs need only the largest blob extra. On "disk full"
   the partial copy is deleted and the error says how much space is
   needed; the store is left as it was.
7. **Progress.** The GUI opens a small "Encrypting your mail" window
   before the workspace: the database step reports the new file's size
   against the old one's (`sqlcipher_export` is a single statement, so
   its output size is the progress signal, polled from another thread),
   then "N of M attachments". It can't be cancelled, but quitting is safe
   (step 5). CLI commands print the same progress to stderr. Measured
   here (§5), a 660 MB store exports in 5 s and checks in about 7 s;
   blobs encrypt at disk speed.
8. Old plaintext blocks stay on the disk until the file system reuses
   them, as with any deletion (see §2.4). Zmail says so once after the
   migration and recommends full-disk encryption.

**If migration can't finish** (disk full, keychain unavailable), Zmail
does not open the plaintext store: it shows the error and Retry, and
never falls back to plaintext (Rob's decision).

### 5. Performance

Measured on the development container (AMD Ryzen 5 3600, shared, load
about 8–13 during the runs, so treat differences under about 30% as
noise), release profile, `performance_budgets` with the 100k-thread
Inbox, on `main` (SQLite 3.50.2), and on the same tree with
`bundled-sqlcipher-vendored-openssl` (SQLCipher 4.6.1 on SQLite 3.46.1)
both with a key and, as a control, without one. Medians, in ms; "enc"
is the encrypted store:

| Measurement | Budget | main (2 runs) | enc (2 runs) | SQLCipher, no key | 5× gate |
| --- | --- | --- | --- | --- | --- |
| Scroll frame, flick (top / middle / end) | 8.33 | 4.4 / 4.4 / 4.4 | 4.3–5.0 / 4.4–5.8 / 4.3–5.5 | 4.9 / 4.5 / 4.3 | 41.7 |
| Scroll frame, scrollbar jump | 8.33 | 12.5–12.6 | 19.6–28.2 | 12.2 | 41.7 |
| Open thread (Select Next) | 100 | 6.0–6.1 | 6.8–13.3 | 6.0 | 500 |
| Mark read / unread | 100 | 33.0 | 44.7–64.1 | 33.2 | 500 |
| Flag / unflag | 100 | 31.7–31.9 | 41.2–44.6 | 31.5 | 500 |
| Archive | 100 | 38.4–41.3 | 52.0–56.9 | 39.6 | 500 |
| Open folder (20k / 100k) | 100 | 15.7–16.0 / 22.8–23.4 | 16.4–18.6 / 24.0–27.3 | 22.7 / 34.2 | 500 |
| Search keystroke, results | 100 | 30.8–32.4 | 36.1–36.9 | 36.5 | 500 |
| Open reader (100 unread / 1 MB HTML / 60 attachments) | 100 | 7.9–8.6 / 35.8–41.3 / 11.3–13.7 | 9.8–11.1 / 41.6–46.0 / 12.3–22.9 | 11.8 / 52.9 / 16.5 | 500 |
| Seeding the store (s) | | 14.0–16.4 | 19.8–21.4 | 15.2 | |

The store benchmarks (`cargo bench -p zmail-store`, divan medians, one
run each, same machine and load) show where the time goes:

| Benchmark | main | enc | SQLCipher, no key |
| --- | --- | --- | --- |
| `large_mailbox_page` at row 0 / 50,000 / 99,850 | 1.19 / 4.04 / 6.87 ms | 2.16 / 8.71 / 15.62 ms | 1.19 / 3.88 / 7.06 ms |
| `large_mailbox_refresh_counts` | 7.54 ms | 23.10 ms | 8.37 ms |
| `thread_view_stress`: long conversation / 1 MB HTML / 60 attachments | 1.28 / 18.28 / 0.77 ms | 2.37 / 20.59 / 1.11 ms | 1.26 / 17.74 / 0.78 ms |
| `full_text_search`, 5,000 threads | 7.21 ms | 11.29 ms | |
| `list_threads_paging`, `address_autocomplete` | 0.78, 0.95 ms | 1.06, 1.31 ms | |
| `snapshot_first_apply`, 5,000 threads (fastest of 100) | 266 ms | 295 ms | |
| `snapshot_one_new_message`, 20,000 threads (fastest of 100) | 571 ms | 625 ms | |

Migration (§4), measured on a store of 60,000 messages with 4 KB bodies
and their search index (660 MB): `sqlcipher_export` wrote the 687 MB
encrypted copy in 5.0 s (133 MB/s), `quick_check` took 4.8 s and
`cipher_integrity_check` 2.4 s. The encrypted file is about 4% larger
(the per-page IV and MAC).

What it shows:

- Everything that met its budget still does. The UI overhead is 10–35%
  on mark read, flag and archive, and about 1.5–2× on the scrollbar
  jump, which reads many cold pages per frame and was already over its
  8.33 ms budget at 1.5×. All are inside the CI gate; the scrollbar jump
  is the closest, at about half the gate.
- Writes cost about 10% more (syncing a snapshot). Reads cost 1.3–3×
  more in the store benchmarks: every page a query touches is decrypted
  and its MAC checked, and the refresh after each action reads many
  pages (7.5 ms to 23 ms).
- The control run shows SQLite 3.46 itself is not the cost.
- Opening a connection costs 0.67 ms on `main` and 0.87 ms encrypted
  (key setup and decrypting the schema pages), and the store opens one
  per call. Each new connection also starts with a cold page cache, so
  every page it reads is decrypted again, even when the OS has the file
  cached. Reusing connections (a small per-thread cache, keeping
  SQLite's page cache warm) would remove most of the read overhead, and
  some of today's cost too. The database PR adds it; the next table
  shows the result.
- CI's runner measured close to these `main` numbers in the run that
  added the gate (#241): scrolling about 25% slower, the actions about
  15% faster, the scrollbar jump 11.3 ms.

**With the connection cache (the database PR).** Each thread keeps its
finished connections per store (at most two), and each connection's page
cache is 16 MiB instead of SQLite's default 2 MiB. The same
`performance_budgets` run, `main` against the encrypted store, two runs
each (medians in ms):

| Measurement | Budget | main | encrypted, with the cache |
| --- | --- | --- | --- |
| Scroll frame, flick (top / middle / end) | 8.33 | 4.4 / 4.4 / 4.3 | 4.4 / 4.4 / 4.4 |
| Scroll frame, scrollbar jump | 8.33 | 12.2–12.5 | 10.7–11.0 |
| Open thread (Select Next) | 100 | 6.0–6.1 | 4.5–5.0 |
| Mark read / unread | 100 | 33.1–33.6 | 17.4–18.7 |
| Flag / unflag | 100 | 31.7–31.9 | 17.2–17.5 |
| Archive | 100 | 38.3–38.4 | 18.6–18.9 |
| Open folder (20k / 100k) | 100 | 13.6–13.8 / 20.6–21.3 | 7.5 / 14.6 |
| Search keystroke, results | 100 | 30.3 | 24.9–25.0 |
| Open reader (100 unread / 1 MB HTML / 60 attachments) | 100 | 7.2 / 36.4–36.6 / 11.0–11.4 | 4.9–5.0 / 33.3 / 9.5 |
| Seeding the store (s) | | 13.6–13.9 | 13.8–13.9 |

The encrypted store with the cache is faster than today's plaintext one
on every measurement, because the plaintext store also paid for a new
connection and a cold page cache on every call. In the store benchmarks
a page at row 99,850 takes 2.5 ms (6.8 ms on `main`) and the refresh
counts 0.16 ms (8.3 ms), though a benchmark repeating one query keeps
its pages warmer than real use does.

### 6. What this does not protect

Encryption at rest protects data **on a disk nobody is logged in to**.
It does not protect against:

- **Anything running as the same user while Zmail can unlock the store.**
  Malware or another process under the user's account can ask the
  keychain for the key (on Linux, Secret Service gives any process of
  the session the secret; on macOS the keychain prompts or asks for the
  login password unless the request comes from Zmail itself; on Windows
  Credential Manager gives it to any process of that user), read Zmail's
  memory, or simply read the opened-attachment copies.
- **Memory.** Decrypted pages, bodies and the key sit in Zmail's process
  memory while it runs. SQLCipher's `cipher_memory_security` (wiping
  and locking its buffers) is off: it costs a lot and protects little
  once an attacker can read the process.
- **Swap and hibernation.** Pages of Zmail's memory, and on Linux the
  tmpfs open cache, can be written to swap or the hibernation image
  unencrypted, unless the OS encrypts those (macOS always does; Windows
  and Linux only with BitLocker or encrypted swap).
- **A disk imaged while the user is logged in**, or a keychain exported
  together with the disk (the keychain is protected by the login
  password; a weak password is a weak key).
- **Plaintext Zmail hands out on purpose**: Save As, Save All, mbox
  export, the clipboard, attachments opened with another app (§2), and
  notifications, whose text the OS keeps in its own notification
  database (macOS, Windows).
- **What leaked before the migration**: blocks of the old plaintext files
  and old backups.
- **Metadata visible from outside the files**: the number and sizes of
  the attachment blobs, file timestamps, and the size of the database.

Full-disk encryption (FileVault, BitLocker, LUKS) covers several of
these (swap, the open cache, old blocks) and is what Zmail recommends
alongside this.

## Consequences

- `zmail-security` gains the `vault` module: the store key and its
  keychain slot (`store-<key id>`, a store-level entry beside the
  per-account `SecretSlot`s), the sidecar, HKDF subkeys and the blob
  format. Direct dependencies on `aes-gcm` (with `stream`), `hkdf`,
  `hmac`, `sha2` and `zeroize`, all already in `Cargo.lock`.
- `LocalStore::open` takes the store key; the app resolves it from the
  keychain (creating it, migrating, or showing recovery). The IMAP and
  JMAP adapters write blobs through the vault.
- New C code: SQLCipher (replacing the plain SQLite amalgamation) and,
  on Linux and Windows, OpenSSL. CI and release need NASM on Windows.
  #254 tracks replacing OpenSSL with a Rust-native backend.
- `zmail-app backup` produces encrypted backups (`VACUUM INTO` keeps the
  key; checked), which only open with the same keychain entry.
- Proton onboarding (#237) may be wired up once the database and file
  PRs are merged.

The implementation stack, each PR "Part of #139":

0. `chore(deps)`: rusqlite 0.37 → 0.40 (SQLCipher 4.14 / SQLite 3.51), #255.
1. Key management: `zmail_security::vault` keys, keychain slot, sidecar,
   HKDF subkeys; no behaviour change yet.
2. Database encryption with migration: SQLCipher features, the key on
   every connection, a per-thread connection cache, `LocalStore::open`
   with a key, the migration and its crash recovery, CLI progress,
   encrypted backups, and the SQLCipher and OpenSSL notices in
   `about.toml` and the `.deb`.
3. File encryption: blob format, the adapters writing through the vault,
   opaque names, draft attachments, the outbox reading through it, the
   open cache (tmpfs on Linux, shorter lifetime), blob migration.
4. UI: the migration progress window and the key-loss recovery screen.
5. Tests: the store opens only with its key; no known message text or
   attachment bytes appear in any file under the data directory (raw
   byte scan of the database, WAL, blobs and sidecar); migration round
   trips; a crash injected at each migration step recovers; and the
   performance budgets (and this ADR's numbers) re-measured.

## Decisions

Taken on October 21, 2026 by Rob and the coordinator, on the questions
this ADR left open:

1. **Dependencies approved**: SQLCipher through rusqlite's
   `bundled-sqlcipher` features, vendored static OpenSSL (`openssl-src`,
   `openssl-sys`) on Linux and Windows, CommonCrypto on macOS, `aes-gcm`'s
   `stream` feature, and NASM in Windows CI and release builds.
2. **Linux uses vendored OpenSSL**, not the system's `libcrypto.so.3`,
   for the reasons in §1.
3. **rusqlite 0.40 first** (#255), so SQLite is not downgraded.
4. **A store that can't be encrypted blocks** with the error and Retry;
   Zmail never falls back to plaintext.
5. **The open cache** keeps a copy for at most an hour while Zmail runs,
   and empties at quit and at the next start.
6. **The per-thread connection cache** ships with the database PR.

## Future work

- A Rust-native crypto backend for the database instead of OpenSSL
  (#254).
- An optional passphrase (§3), which is also the answer for Linux
  without a Secret Service.
- A Settings button for key rotation (the command ships in this stack).
