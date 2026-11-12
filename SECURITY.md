# Security policy

## Supported versions

Zmail is alpha software. Only the latest release receives security fixes; please update before reporting, and check that the problem is still there.

## Reporting a vulnerability

Please report it privately, not in a public issue or pull request. Use GitHub's private vulnerability reporting: open the [Security tab](https://github.com/unicornops/zmail/security) of this repository and choose "Report a vulnerability". If that option is missing, open a public issue that says only that you have a security report and want a private channel, with no details, and a maintainer will reply there.

Please include:

- the Zmail version and platform (the release tag, and whether it is the desktop app or `zmail-app tui`)
- what you found and what an attacker gains from it
- steps or a sample message that reproduce it (a minimal `.eml` or HTML file is ideal)
- whether it needs a particular account type (IMAP, JMAP or Proton) or setting

Please give us a reasonable time to fix a problem before you publish it. Zmail is a side project, so there is no guaranteed response time, but a report will be read and acknowledged.

## In scope

- The HTML sanitiser and the other rendering paths for mail (the Simple view, the web renderers and the terminal's escape-sequence filtering): anything that lets a message run script, load remote content when it is blocked, reach local files or tamper with what the reader shows.
- Sender-safety and attachment-safety checks that can be bypassed.
- Protocol code: IMAP, JMAP, SMTP, OAuth2 sign-in and the native Proton Mail client, including TLS and certificate handling and the OpenPGP and SRP code.
- Local store encryption and secret handling: the encrypted store, attachment blobs, key and password storage in the OS keychain.

## Out of scope

- Anything that needs an attacker who already runs code as your user or can read Zmail's memory (see "What this does not protect" in [ADR 0005](./docs/adr/0005-store-encryption.md)).
- Problems in a mail provider's servers, or in Proton's private API.
- Vulnerabilities in dependencies that Zmail does not reach; report those upstream (advisories are checked in CI with `cargo deny`).
- Unsigned Windows and Linux downloads: this is known and documented in the README.
