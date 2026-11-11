//! What Zmail knows about the providers it has presets for: which one an
//! address belongs to, and the help each one's sign-in needs.
//!
//! The servers themselves are `zmail_autodiscover::Provider::settings`;
//! this module adds the rules around them that both frontends show.

use zmail_autodiscover::{EmailAddress, Provider};

use crate::google;

/// Where to learn how to make an app password, for a provider that needs
/// one: what to say, and the provider's help page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppPasswordHelp {
    /// The hint, shown above the password field.
    pub text: &'static str,
    /// The provider's help page.
    pub url: &'static str,
}

/// The app password hint for `provider`, if it needs one. Microsoft 365
/// refuses passwords altogether, so it has none.
pub fn app_password_help(provider: Provider) -> Option<AppPasswordHelp> {
    let (text, url) = match provider {
        Provider::Google => (
            "Google needs an app password, not your Google password. App passwords need 2-Step Verification turned on for your Google account; then create one for Zmail.",
            google::APP_PASSWORDS_HELP_URL,
        ),
        Provider::ICloud => (
            "iCloud needs an app-specific password, not your Apple Account password. Create one for Zmail in your Apple Account settings.",
            "https://support.apple.com/en-us/102654",
        ),
        Provider::Fastmail => (
            "Fastmail needs an app password, not your Fastmail password. Create one with mail access in Fastmail's settings.",
            "https://www.fastmail.help/hc/en-us/articles/360058752854",
        ),
        Provider::Microsoft365 => return None,
    };
    Some(AppPasswordHelp { text, url })
}

/// The provider's one-line description, as the provider step lists it.
pub fn provider_hint(provider: Provider) -> &'static str {
    match provider {
        Provider::Google => "Gmail and Google Workspace",
        // The next step needs an OAuth client ID from an organisation,
        // which a personal Outlook.com user doesn't have (#269).
        Provider::Microsoft365 => {
            "Work or school accounts, with your organisation's OAuth client ID; not personal Outlook.com yet"
        }
        Provider::Fastmail => "Fastmail, over JMAP",
        Provider::ICloud => "iCloud Mail",
    }
}

/// The provider whose own consumer domain `email` is at, if any: the
/// preset applies without asking the network. Addresses at other domains
/// (Google Workspace or Microsoft 365 on a company's domain, say) go
/// through autodiscovery, whose MX step recognises the same providers.
pub fn provider_for_address(email: &EmailAddress) -> Option<Provider> {
    let domain = email.domain().trim_end_matches('.').to_ascii_lowercase();
    match domain.as_str() {
        "gmail.com" | "googlemail.com" => Some(Provider::Google),
        "outlook.com" | "hotmail.com" | "live.com" | "msn.com" | "outlook.ie" | "hotmail.co.uk"
        | "live.co.uk" => Some(Provider::Microsoft365),
        "fastmail.com" | "fastmail.fm" | "fastmail.net" | "fastmail.org" => {
            Some(Provider::Fastmail)
        }
        "icloud.com" | "me.com" | "mac.com" => Some(Provider::ICloud),
        _ => None,
    }
}

/// Whether `email` is at one of Proton's own domains, where the account
/// is added by signing in to Proton rather than with server settings.
/// Proton addresses on a custom domain can't be told apart this way; the
/// frontends also offer Proton Mail as a choice of its own.
pub fn is_proton_address(email: &EmailAddress) -> bool {
    let domain = email.domain().trim_end_matches('.').to_ascii_lowercase();
    matches!(
        domain.as_str(),
        "proton.me" | "protonmail.com" | "protonmail.ch" | "pm.me"
    )
}

#[cfg(test)]
mod tests {
    use zmail_autodiscover::{EmailAddress, Provider};

    use super::{app_password_help, is_proton_address, provider_for_address};

    fn address(text: &str) -> EmailAddress {
        EmailAddress::parse(text).expect("a valid address")
    }

    #[test]
    fn consumer_domains_pick_their_preset() {
        assert_eq!(
            provider_for_address(&address("jane@gmail.com")),
            Some(Provider::Google)
        );
        assert_eq!(
            provider_for_address(&address("Jane@GoogleMail.com")),
            Some(Provider::Google)
        );
        assert_eq!(
            provider_for_address(&address("jane@outlook.com")),
            Some(Provider::Microsoft365)
        );
        assert_eq!(
            provider_for_address(&address("jane@fastmail.com")),
            Some(Provider::Fastmail)
        );
        assert_eq!(
            provider_for_address(&address("jane@me.com")),
            Some(Provider::ICloud)
        );
        // Everything else is discovered, including look-alikes.
        for other in [
            "jane@example.com",
            "jane@gmail.com.evil.example",
            "jane@mail.gmail.com",
        ] {
            assert_eq!(provider_for_address(&address(other)), None, "{other}");
        }
    }

    #[test]
    fn proton_domains_are_recognised() {
        assert!(is_proton_address(&address("jane@proton.me")));
        assert!(is_proton_address(&address("jane@pm.me")));
        assert!(!is_proton_address(&address("jane@notproton.me")));
    }

    #[test]
    fn app_password_hints_link_to_the_provider() {
        for provider in [Provider::Google, Provider::ICloud, Provider::Fastmail] {
            let help = app_password_help(provider).expect("a hint");
            assert!(help.url.starts_with("https://"), "{provider}");
            assert!(help.text.contains("password"), "{provider}");
        }
        assert!(
            app_password_help(Provider::Microsoft365).is_none(),
            "Microsoft 365 refuses passwords"
        );
    }
}
