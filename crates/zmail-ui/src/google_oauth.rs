//! Bringing your own Google OAuth client: the guide's steps, its links,
//! and what zmail says when the client's sign-in expires.
//!
//! zmail has no Google client of its own (a verified one is #268), so
//! Gmail users either use an app password (the default) or create a
//! Desktop client in their own Google Cloud project. The onboarding guide
//! (`account_setup/google_guide.rs`) and `docs/gmail-oauth.md` walk
//! through the same steps; keep them in step with each other.
//!
//! The trap: while the project's app is in **Testing**, Google ends its
//! sign-ins after 7 days and every refresh answers `invalid_grant`.
//! Publishing the app, or choosing Internal on Google Workspace, avoids
//! it. Settings → Accounts explains this when it happens
//! ([`EXPIRED_ADVICE`]).

/// The guide in the repository, linked from the app.
pub(crate) const GUIDE_URL: &str =
    "https://github.com/unicornops/zmail/blob/main/docs/gmail-oauth.md";

/// Where Google account holders create an app password.
pub(crate) const APP_PASSWORDS_URL: &str = "https://myaccount.google.com/apppasswords";

/// Google's help on app passwords and 2-Step Verification.
pub(crate) const APP_PASSWORDS_HELP_URL: &str = "https://support.google.com/accounts/answer/185833";

/// The one scope IMAP and SMTP accept for XOAUTH2.
pub(crate) const MAIL_SCOPE: &str = "https://mail.google.com/";

/// The end of every Google OAuth client ID.
pub(crate) const CLIENT_ID_SUFFIX: &str = ".apps.googleusercontent.com";

/// The start of the client secrets Google issues today.
const CLIENT_SECRET_PREFIX: &str = "GOCSPX-";

/// One step of the guide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuideStep {
    /// A stable id for selectors.
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) body: &'static str,
    /// The Google Cloud console page the step happens on, if any.
    pub(crate) link: Option<(&'static str, &'static str)>,
}

/// The guide's steps in order. The last one happens in zmail.
pub(crate) const GUIDE_STEPS: [GuideStep; 6] = [
    GuideStep {
        id: "project",
        title: "Create a Google Cloud project",
        body: "Name it anything, for example zmail. No billing account is needed.",
        link: Some((
            "Create project",
            "https://console.cloud.google.com/projectcreate",
        )),
    },
    GuideStep {
        id: "branding",
        title: "Set up branding",
        body: "In Google Auth Platform, choose Get started if it asks. Enter an app name, such as zmail, and your email address as the support and contact email.",
        link: Some((
            "Open Branding",
            "https://console.cloud.google.com/auth/branding",
        )),
    },
    GuideStep {
        id: "audience",
        title: "Choose the audience",
        body: "Choose External and add your own Gmail address as a test user. On Google Workspace you can choose Internal instead: no warning screen, and sign-ins don't expire after 7 days.",
        link: Some((
            "Open Audience",
            "https://console.cloud.google.com/auth/audience",
        )),
    },
    GuideStep {
        id: "scopes",
        title: "Allow mail access",
        body: "Under Data access, choose Add or remove scopes and add https://mail.google.com/. If it isn't listed, paste it under Manually add scopes. It is the only scope Gmail's IMAP and SMTP accept.",
        link: Some((
            "Open Data access",
            "https://console.cloud.google.com/auth/scopes",
        )),
    },
    GuideStep {
        id: "client",
        title: "Create a Desktop app client",
        body: "Under Clients, choose Create client, pick Desktop app as the application type, and create it. Copy the client ID and the client secret straight away: Google shows the secret only once.",
        link: Some((
            "Open Clients",
            "https://console.cloud.google.com/auth/clients",
        )),
    },
    GuideStep {
        id: "paste",
        title: "Paste them into zmail",
        body: "Enter your Gmail address, the client ID and the client secret below, then continue. Google asks you to approve access in your browser.",
        link: None,
    },
];

/// The guide's warning about Testing apps.
pub(crate) const TESTING_EXPIRY: &str = "While the app's publishing status is Testing, Google ends zmail's sign-in after 7 days and you have to sign in again. To avoid that, publish the app under Audience (Google then warns that it is unverified, which you can accept for your own app), or on Google Workspace choose Internal.";

/// What Settings says when a Google account's saved sign-in stops working.
pub(crate) const EXPIRED_ADVICE: &str = "Google no longer accepts zmail's saved sign-in. If your Google Cloud app is still in Testing, Google ends its sign-ins after 7 days. Publish the app under Audience, or on Google Workspace make it Internal, then sign in again so it lasts.";

/// Checks a pasted client ID, returning what is wrong with it.
pub(crate) fn check_client_id(client_id: &str) -> Option<&'static str> {
    let client_id = client_id.trim();
    if client_id.starts_with(CLIENT_SECRET_PREFIX) {
        return Some(
            "That looks like the client secret. The client ID ends in .apps.googleusercontent.com.",
        );
    }
    let shaped = client_id
        .strip_suffix(CLIENT_ID_SUFFIX)
        .is_some_and(|prefix| {
            !prefix.is_empty()
                && prefix
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        });
    (!shaped).then_some(
        "A Google client ID ends in .apps.googleusercontent.com. Copy it from the client's page under Clients.",
    )
}

/// Checks a pasted client secret, returning what is wrong with it.
pub(crate) fn check_client_secret(client_secret: &str) -> Option<&'static str> {
    let client_secret = client_secret.trim();
    if client_secret.is_empty() {
        return Some(
            "Google needs the client secret as well as the ID. Copy it from the client in Google Cloud.",
        );
    }
    if client_secret.ends_with(CLIENT_ID_SUFFIX) {
        return Some("That is the client ID. Paste the client secret here.");
    }
    if client_secret.chars().any(char::is_whitespace) {
        return Some("A client secret has no spaces. Copy it again from Google Cloud.");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{GUIDE_STEPS, check_client_id, check_client_secret};

    #[test]
    fn client_ids_must_be_google_shaped() {
        for good in [
            "123456789012-abcdefghijklmnop0123456789abcdef.apps.googleusercontent.com",
            "  42-x.apps.googleusercontent.com  ",
        ] {
            assert_eq!(check_client_id(good), None, "{good}");
        }
        for bad in [
            "",
            ".apps.googleusercontent.com",
            "123-abc",
            "123-abc.apps.googleusercontent.com.evil.example",
            "123 abc.apps.googleusercontent.com",
            "https://123-abc.apps.googleusercontent.com",
        ] {
            assert!(check_client_id(bad).is_some(), "{bad:?}");
        }
        assert!(
            check_client_id("GOCSPX-abcdef")
                .is_some_and(|message| message.contains("client secret")),
            "a pasted secret is named"
        );
    }

    #[test]
    fn client_secrets_must_be_present() {
        assert_eq!(check_client_secret("GOCSPX-abc_DEF-123"), None);
        assert_eq!(check_client_secret("legacy-secret"), None);
        assert!(check_client_secret("").is_some());
        assert!(check_client_secret("   ").is_some());
        assert!(check_client_secret("abc def").is_some());
        assert!(
            check_client_secret("1-x.apps.googleusercontent.com")
                .is_some_and(|message| message.contains("client ID")),
            "a pasted ID is named"
        );
    }

    #[test]
    fn the_guide_links_to_the_google_cloud_console_only() {
        assert_eq!(GUIDE_STEPS.len(), 6);
        for step in GUIDE_STEPS {
            if let Some((_, url)) = step.link {
                assert!(
                    url.starts_with("https://console.cloud.google.com/"),
                    "{url}"
                );
            }
        }
        assert!(GUIDE_STEPS[5].link.is_none(), "the last step is in zmail");
        assert!(GUIDE_STEPS[3].body.contains(super::MAIL_SCOPE));
    }
}
