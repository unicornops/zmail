# Gmail with your own Google OAuth client

zmail has no Google OAuth client of its own yet (a verified one is tracked
in [#268](https://github.com/unicornops/zmail/issues/268)). Gmail accounts
can sign in two ways:

- **An app password (the default, and quicker).** It needs
  [2-Step Verification](https://support.google.com/accounts/answer/185833)
  on your Google account. Then
  [create an app password](https://myaccount.google.com/apppasswords) and
  paste it into zmail's password field.
- **A Google OAuth client of your own.** Use this when app passwords are
  not available to you, for example when a Google Workspace administrator
  has turned them off. It takes about ten minutes, once, and costs
  nothing. zmail never sees your Google password: Google asks you to
  approve access in your browser.

In zmail, pick **Google** when adding an account, then choose **Set up
your own client** under the app password note. The in-app guide has the
same steps as this page, with buttons that open each Google Cloud page.

## Steps

1. **Create a Google Cloud project.**
   Open [Create project](https://console.cloud.google.com/projectcreate)
   and name it anything, for example `zmail`. No billing account is
   needed.
2. **Set up branding.**
   Open [Branding](https://console.cloud.google.com/auth/branding) (choose
   **Get started** if Google Auth Platform asks). Enter an app name such as
   `zmail`, and your email address as the user support email and the
   developer contact email.
3. **Choose the audience.**
   Open [Audience](https://console.cloud.google.com/auth/audience).
   - Choose **External** and add your own Gmail address under **Test
     users**.
   - On Google Workspace you can choose **Internal** instead. Only people
     in your organisation can then use the client, there is no "unverified
     app" warning, and sign-ins do not expire after 7 days (see below).
4. **Allow mail access.**
   Open [Data access](https://console.cloud.google.com/auth/scopes), choose
   **Add or remove scopes** and add `https://mail.google.com/`. If it is
   not in the list, paste it under **Manually add scopes**. It is the only
   scope Gmail's IMAP and SMTP servers accept for OAuth (XOAUTH2); you do
   not need to enable the Gmail API.
5. **Create a Desktop app client.**
   Open [Clients](https://console.cloud.google.com/auth/clients), choose
   **Create client**, pick **Desktop app** as the application type, and
   create it. Copy the **client ID** and the **client secret** straight
   away: Google shows the secret only once (you can add a new secret to
   the client later if you lose it).
6. **Paste them into zmail.**
   Enter your Gmail address, the client ID (it ends in
   `.apps.googleusercontent.com`) and the client secret, then continue.
   zmail opens Google's sign-in page in your browser; approve access, and
   the account starts syncing.

zmail keeps the client secret, the access token and the refresh token in
your system keychain. The secret is sent only to Google's token endpoint,
never in the browser's address bar.

## The 7-day trap

While the app's **publishing status** is **Testing** (the default for an
External app), Google lets its sign-ins last only 7 days. After that every
attempt to refresh the sign-in fails with `invalid_grant`, and zmail shows
"sign-in has expired" on the account with a **Re-authenticate** button.

To stop that happening, pick one:

- **Publish the app.** On the
  [Audience](https://console.cloud.google.com/auth/audience) page choose
  **Publish app**. You do not need Google's verification for your own
  use: the consent screen then warns that the app is unverified, and you
  can continue past it because you are its developer. An unverified app
  is limited to 100 users, which is plenty for one person.
- **Make it Internal** (Google Workspace only), as in step 3.

Then choose **Re-authenticate** on the account in **Settings → Accounts**
once more. The new sign-in lasts until you revoke it.

## Troubleshooting

- **"That looks like the client secret"** or **"That is the client ID"**:
  the two were pasted the wrong way round.
- **`invalid_grant` / "sign-in has expired"**: see the 7-day trap above,
  then Re-authenticate. It also happens if you removed zmail's access at
  [Third-party access](https://myaccount.google.com/connections).
- **"Access blocked: zmail has not completed the Google verification
  process"**: your Gmail address is not a test user. Add it under
  Audience → Test users, or publish the app.
- **`client_secret is missing`**: the secret field was left empty. Google
  Desktop clients need it at the token exchange.

Do not reuse another application's Google client ID (Thunderbird's,
GNOME's or any other). It is against Google's terms, and those projects
ask people not to.
