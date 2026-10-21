# OAuth2 live-provider verification runbook

This runbook covers issue #89: the OAuth2 flows are verified against
loopback mocks (unit tests) and the password-based James harness (live
tests), but the real-provider round trips must be exercised manually with
actual accounts before the OAuth2 paths can be called done.

A code-path audit (2026-10) found the implementation complete and covered
by tests: provider presets (`Gmail`, `Microsoft 365`) wire IMAP and SMTP
endpoints, SMTP submits with `AUTH XOAUTH2`, JMAP authenticates with a
bearer token, and the access token refreshes in the background
(`zmail-auth` presets, `zmail-smtp` XOAUTH2, `zmail-jmap` bearer
credentials, `zmail-sync` `connect_secret`). Anything found during the
runs below should be filed as a follow-up issue.

## Gmail (IMAP + SMTP over XOAUTH2)

Prerequisites: a Google account, and a Google Cloud OAuth client
configured as a **desktop** application with the
`https://mail.google.com/` scope ([gmail-oauth.md](gmail-oauth.md) has
the steps). Google requires the client's secret at
the token exchange even for desktop clients (PKCE is used on top), so
zmail asks for it. The secret is sent only to the token endpoint, never
in the browser's authorization URL.

1. Launch Zmail, open **Add Account**, pick the **Gmail** preset, enter
   the client id and secret, and complete the browser consent on the
   loopback redirect. The preset fills `imaps://imap.gmail.com:993` and
   `smtp.gmail.com:465` (implicit TLS).
2. Sync the account; confirm messages land in the local mirror and the
   IDLE watcher reports new mail.
3. Compose and send a message; confirm Gmail accepts the submission with
   `AUTH XOAUTH2` (no password prompt, message arrives at the recipient).
4. With the app left in Testing, wait 7 days: the refresh fails with
   `invalid_grant`, and Settings → Accounts says the sign-in has expired,
   with Re-authenticate and the guide's fix.
5. Let the access token expire (or use a test client with a short token
   lifetime). Confirm background refresh keeps sync, IDLE, and sending
   working without user interaction.
6. Revoke the grant at `myaccount.google.com > Security > Third-party
   access`, trigger a sync, and confirm the failure surfaces as a sync
   failure status in the UI rather than a hang.

## Microsoft 365 (IMAP + SMTP over XOAUTH2)

Prerequisites: a Microsoft 365 or Outlook.com account, and an Entra app
registration as a public client (no secret) with a loopback redirect. The
preset asks for `https://outlook.office.com/IMAP.AccessAsUser.All`,
`https://outlook.office.com/SMTP.Send` and `offline_access`; the tenant
must allow SMTP AUTH for the mailbox.

1. Add the account with the **Microsoft 365** preset and the client id
   under Advanced, and complete the consent, which lists reading and
   sending mail.
2. Sync, then send a message; confirm `smtp.office365.com:587` accepts
   `AUTH XOAUTH2` with the same token.
3. An account signed in before zmail asked for `SMTP.Send` keeps its
   old grant: a send is refused, the Outbox row says "Sign in again to
   send" with **Re-authenticate**, and signing in again (which shows the
   consent for sending) sends the waiting message.

## JMAP provider (Fastmail or similar) with OAuth2

Prerequisites: a Fastmail account with an app/OAuth2 client (Fastmail's
developer token flow), or any JMAP provider with an OAuth2 endpoint.

1. Open **Add Account**, pick JMAP with OAuth2 auth, and enter the
   provider's authorization and token endpoints (no consumer preset
   covers JMAP; the dialog notes this).
2. Complete the browser flow; confirm the account syncs with bearer
   authentication.
3. Confirm event-source push delivers new mail without a manual sync.
4. Compose and send; confirm `EmailSubmission` succeeds end to end.
5. Revoke the token at the provider and confirm the failure surfaces as
   a sync failure status rather than a hang.

## Reporting results

- Record the account provider, the rough duration, and any error text
  verbatim.
- If the app crashes during any step, attach the crash log line from the
  platform crash log (`~/Library/Logs/zmail/crash.log` on macOS; see the
  crash policy in [phase5-hardening.md](phase5-hardening.md)).
- File follow-up issues for every rough edge and link them from #89;
  close #89 only when both acceptance criteria have been demonstrated.
