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

`live_jmap_harness` starts the harness, syncs mail from the proxied James JMAP endpoint, submits new mail through SMTP, and confirms the next JMAP sync sees the new message. Its push tests open the James event source and WebSocket push (RFC 8620 section 7.3, RFC 8887) through the nginx proxy, which passes the WebSocket upgrade through and does not buffer, and check that a running mail watcher brings pushed mail and remote keyword changes into the store through `Email/changes` without a manual sync.

## Dovecot CONDSTORE service

The compose stack also builds a plain-IMAP Dovecot service (`docker/mail-harness/dovecot`) exposing `imap://127.0.0.1:3243` with a single static account (`mailuser` / `mailuser-password`). Dovecot advertises CONDSTORE and QRESYNC, so `live_mail_harness`'s `syncs_flags_incrementally_through_condstore` test exercises the RFC 7162 incremental flag path: an unchanged second sync fetches no flag data, a flag change made by another client arrives through `CHANGEDSINCE`, and GreenMail (non-CONDSTORE) keeps working through the full flag scan. Because Dovecot also advertises QRESYNC, that test's unchanged syncs report `via QRESYNC`, and `resyncs_expunges_and_flags_through_qresync` checks that a message expunged and a message flagged by another client arrive through the `VANISHED (EARLIER)` and `FETCH ... MODSEQ` responses of a single QRESYNC `EXAMINE`; it skips (and says so) if the server stops advertising QRESYNC. GreenMail advertises neither CONDSTORE nor QRESYNC.

## Live updates on every folder

`live_mail_harness` also covers the IMAP watchers. Against GreenMail (no NOTIFY), `idle_and_status_watchers_report_changes_beyond_the_inbox` checks the pool plan, an IDLE connection on INBOX when GreenMail advertises IDLE, and the `STATUS` poller reporting a message appended to another folder. The Dovecot config enables `mailbox_list_index`, which Dovecot needs to advertise NOTIFY (RFC 5465): `notify_reports_changes_in_any_mailbox` checks that one NOTIFY connection reports a message appended to a non-INBOX folder (it skips, and says so, if NOTIFY is not advertised), and `mail_watcher_syncs_mail_delivered_to_any_folder` checks that a running mail watcher brings such a message into the store without a manual sync.
