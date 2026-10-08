# Mail Harness

This directory contains the Phase 2b live mail harness used for integration testing.

It starts:

- SMTP on `127.0.0.1:3025`
- IMAP on `127.0.0.1:3143`
- JMAP on `http://127.0.0.1:4080`
- a seeded inbox for `phase2@localhost`
- authenticated SMTP submission for `sender@localhost`
- a provisioned JMAP mailbox for `jmap@localhost`

Useful commands:

```bash
scripts/mail-harness.sh up
scripts/mail-harness.sh ps
scripts/mail-harness.sh logs
scripts/mail-harness.sh down
```

The main end-to-end verification path is:

```bash
cargo test -p zmail-test-support --test live_mail_harness -- --ignored --nocapture
cargo test -p zmail-test-support --test live_jmap_harness -- --ignored --nocapture
```

The harness wrapper provisions the James JMAP users automatically on `up` and `reset`.

`live_mail_harness` starts the harness, syncs the preloaded IMAP mail, submits a message through SMTP, and confirms the next IMAP sync sees the new message.

`live_jmap_harness` starts the harness, syncs mail from the proxied James JMAP endpoint, submits new mail through SMTP, and confirms the next JMAP sync sees the new message.

## Dovecot CONDSTORE service

The compose stack also builds a plain-IMAP Dovecot service (`docker/mail-harness/dovecot`) exposing `imap://127.0.0.1:3243` with a single static account (`mailuser` / `mailuser-password`). Dovecot advertises CONDSTORE and QRESYNC, so `live_mail_harness`'s `syncs_flags_incrementally_through_condstore` test exercises the RFC 7162 incremental flag path: an unchanged second sync fetches no flag data, a flag change made by another client arrives through `CHANGEDSINCE`, and GreenMail (non-CONDSTORE) keeps working through the full flag scan.
