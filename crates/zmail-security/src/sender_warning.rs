//! What a sender-safety warning says (#108), for both frontends: the
//! desktop app's reader banner and the terminal's Message Text show the
//! same title, sentence and evidence from [`analyze_sender_identity`].
//!
//! | Finding | Tier |
//! |---|---|
//! | The From domain imitates a well-known domain (look-alike letters from any alphabet included) | Danger |
//! | The display name shows an address or website on another domain | Caution |
//! | The From domain mixes alphabets in one label (Latin and Cyrillic, say) | Caution |
//! | Reply-To is on an unrelated domain | Caution |
//! | Reply-To is on a parent or subdomain of the From domain | Info |
//! | The From domain is written with international letters | Info |
//!
//! One banner shows, at the highest tier, worded after the first finding
//! of that tier in this table; its evidence lists every finding's.
//!
//! "Looks safe" hides the warning for that message only (stored with the
//! message, `LocalStore::dismiss_sender_warning`). With the sender
//! warnings setting on "Fewer warnings", Info does not show; Caution and
//! Danger always do. The strings come from mail: a terminal filters them
//! before drawing them.

use zmail_core::{
    ThreadMessage,
    participants::{parse_address_list, sender_identity},
    preferences::SenderWarnings,
};

use crate::sender_safety::{SafetyTier, SenderSafetyReport, analyze_sender_identity};

/// Whether `message`'s sender warrants a warning above Info (a look-alike
/// domain, or a Reply-To elsewhere), whatever the warning level setting
/// and even after "Looks safe": such a sender is never trusted for good.
pub fn sender_is_suspect(message: &ThreadMessage) -> bool {
    analyze_sender_identity(&message.sender, message.reply_to.as_deref())
        .tier()
        .is_some_and(|tier| tier > SafetyTier::Info)
}

/// What a sender-safety banner says: its tier, title, sentence and the
/// evidence under "Show details".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderWarning {
    /// How serious it is.
    pub tier: SafetyTier,
    /// Its title ("Possible impersonation").
    pub title: &'static str,
    /// The sentence under the title.
    pub message: String,
    /// The evidence ("Show details" in the desktop app): a label and the
    /// address or domain from the mail.
    pub details: Vec<(&'static str, String)>,
}

/// The warning for `message`, or `None` when there is nothing to show at
/// `level`.
pub fn sender_warning(message: &ThreadMessage, level: SenderWarnings) -> Option<SenderWarning> {
    if message.sender_warning_dismissed {
        return None;
    }
    let report = analyze_sender_identity(&message.sender, message.reply_to.as_deref());
    describe(&report, &message.sender, message.reply_to.as_deref())
        .filter(|warning| shows(warning.tier, level))
}

/// Whether a warning of `tier` shows at `level`. Only Info can be left
/// out: lookalike senders (Danger) and an unrelated Reply-To (Caution)
/// always show.
pub fn shows(tier: SafetyTier, level: SenderWarnings) -> bool {
    match level {
        SenderWarnings::Standard => true,
        SenderWarnings::Fewer => tier >= SafetyTier::Caution,
    }
}

fn describe(
    report: &SenderSafetyReport,
    from: &str,
    reply_to: Option<&str>,
) -> Option<SenderWarning> {
    let tier = report.tier()?;
    let from_domain = report.from_domain.clone().unwrap_or_default();
    let from_address = sender_identity(from).1.unwrap_or_else(|| from.to_owned());
    let reply_to_addresses = reply_to
        .map(|reply_to| {
            parse_address_list(reply_to)
                .into_iter()
                .filter_map(|participant| participant.email)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|addresses| !addresses.is_empty());
    let international = report.international_domain.as_ref();
    let mixed = international.filter(|domain| domain.mixed);

    // The banner names the most serious finding; the evidence lists every
    // finding's.
    let (title, message) = match tier {
        SafetyTier::Danger => {
            let known = report.lookalike_of.clone().unwrap_or_default();
            (
                "Possible impersonation",
                format!(
                    "The sender's domain imitates {known}, so this message may not be from them."
                ),
            )
        }
        SafetyTier::Caution => {
            if let Some(shown) = &report.name_shows {
                (
                    "Name shows another address",
                    format!(
                        "The sender's name shows {shown}, but the message comes from {from_domain}."
                    ),
                )
            } else if mixed.is_some() {
                (
                    "Mixed alphabets in the sender's domain",
                    format!(
                        "The sender's domain mixes letters from different alphabets, which can make it look like another domain. Its plain form is {from_domain}."
                    ),
                )
            } else {
                let reply_domain = report.reply_to_mismatch.clone().unwrap_or_default();
                (
                    "Replies go somewhere else",
                    format!(
                        "Replying sends your message to {reply_domain}, not to the sender's domain."
                    ),
                )
            }
        }
        SafetyTier::Info => {
            if let Some(reply_domain) = &report.reply_to_mismatch {
                (
                    "Replies go to a related address",
                    format!(
                        "Replies go to {reply_domain}, a related domain, rather than {from_domain}."
                    ),
                )
            } else {
                (
                    "International sender domain",
                    format!(
                        "The sender's domain uses international letters. Its plain form is {from_domain}."
                    ),
                )
            }
        }
    };

    let mut details = vec![("From", from_address)];
    if let Some(shown) = &report.name_shows {
        details.push(("Name shows", shown.clone()));
    }
    if report.lookalike_of.is_some() || report.name_shows.is_some() || international.is_some() {
        details.push(("Sender's domain", from_domain));
    }
    if let Some(known) = &report.lookalike_of {
        details.push(("Looks like", known.clone()));
    }
    if let Some(domain) = international {
        details.push(("Alphabets", domain.scripts.join(", ")));
    }
    if report.reply_to_mismatch.is_some()
        && let Some(addresses) = reply_to_addresses
    {
        details.push(("Replies go to (Reply-To)", addresses));
    }
    Some(SenderWarning {
        tier,
        title,
        message,
        details,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fewer_warnings_leaves_out_info_only() {
        for tier in [SafetyTier::Info, SafetyTier::Caution, SafetyTier::Danger] {
            assert!(shows(tier, SenderWarnings::Standard));
        }
        assert!(!shows(SafetyTier::Info, SenderWarnings::Fewer));
        assert!(shows(SafetyTier::Caution, SenderWarnings::Fewer));
        assert!(shows(SafetyTier::Danger, SenderWarnings::Fewer));
    }

    fn warning(from: &str, reply_to: Option<&str>) -> Option<SenderWarning> {
        let report = analyze_sender_identity(from, reply_to);
        describe(&report, from, reply_to)
    }

    #[test]
    fn safe_senders_get_no_banner() {
        assert_eq!(warning("Kai <kai@example.com>", None), None);
        assert_eq!(
            warning("Kai <kai@example.com>", Some("kai@example.com")),
            None
        );
    }

    #[test]
    fn lookalike_domains_are_danger_with_both_domains_as_evidence() {
        let warning =
            warning("PayPal Security <security@paypa1.com>", None).expect("a lookalike is flagged");
        assert_eq!(warning.tier, SafetyTier::Danger);
        assert_eq!(warning.title, "Possible impersonation");
        assert!(warning.message.contains("paypal.com"));
        assert_eq!(
            warning.details,
            vec![
                ("From", "security@paypa1.com".to_owned()),
                ("Sender's domain", "paypa1.com".to_owned()),
                ("Looks like", "paypal.com".to_owned()),
            ]
        );
    }

    #[test]
    fn unrelated_reply_to_is_caution_with_both_addresses() {
        let warning = warning(
            "Billing <billing@example.com>",
            Some("Billing <collect@evil.example>"),
        )
        .expect("the mismatch is flagged");
        assert_eq!(warning.tier, SafetyTier::Caution);
        assert!(warning.message.contains("evil.example"));
        assert_eq!(
            warning.details,
            vec![
                ("From", "billing@example.com".to_owned()),
                (
                    "Replies go to (Reply-To)",
                    "collect@evil.example".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn related_reply_to_is_info() {
        let warning = warning("news@example.com", Some("reply@lists.example.com"))
            .expect("the related domain is still shown");
        assert_eq!(warning.tier, SafetyTier::Info);
        assert_eq!(warning.title, "Replies go to a related address");
    }

    #[test]
    fn lookalike_and_reply_to_show_one_danger_banner_with_all_evidence() {
        let warning = warning("security@paypa1.com", Some("evil@evil.example")).expect("flagged");
        assert_eq!(warning.tier, SafetyTier::Danger);
        assert_eq!(
            warning.details.last(),
            Some(&("Replies go to (Reply-To)", "evil@evil.example".to_owned()))
        );
    }

    #[test]
    fn a_name_showing_another_address_is_caution_with_both_domains() {
        let warning =
            warning("\"ceo@corp.example\" <boss@evil.example>", None).expect("the name is flagged");
        assert_eq!(warning.tier, SafetyTier::Caution);
        assert_eq!(warning.title, "Name shows another address");
        assert_eq!(
            warning.message,
            "The sender's name shows corp.example, but the message comes from evil.example."
        );
        assert_eq!(
            warning.details,
            vec![
                ("From", "boss@evil.example".to_owned()),
                ("Name shows", "corp.example".to_owned()),
                ("Sender's domain", "evil.example".to_owned()),
            ]
        );
    }

    #[test]
    fn international_domains_show_their_plain_form() {
        let mixed = warning("Bank <x@b\u{0430}nk.example>", None).expect("mixed alphabets");
        assert_eq!(mixed.tier, SafetyTier::Caution);
        assert_eq!(mixed.title, "Mixed alphabets in the sender's domain");
        assert!(mixed.message.contains("xn--"), "{}", mixed.message);
        assert!(
            mixed
                .details
                .contains(&("Alphabets", "Latin, Cyrillic".to_owned()))
        );

        let international =
            warning("Hans <hans@m\u{00FC}ller.example>", None).expect("international");
        assert_eq!(international.tier, SafetyTier::Info);
        assert_eq!(international.title, "International sender domain");
        assert_eq!(
            international.message,
            "The sender's domain uses international letters. Its plain form is xn--mller-kva.example."
        );
        assert!(!shows(international.tier, SenderWarnings::Fewer));

        // A Cyrillic look-alike of a well-known domain is impersonation.
        let lookalike = warning("Apple <id@\u{0430}pple.com>", None).expect("a lookalike");
        assert_eq!(lookalike.tier, SafetyTier::Danger);
        assert!(
            lookalike
                .details
                .contains(&("Looks like", "apple.com".to_owned()))
        );
    }

    #[test]
    fn a_comma_in_the_name_does_not_hide_the_warning() {
        let warning = warning(
            "\"PayPal, Inc.\" <support@paypa1.com>",
            Some("\"Billing, Dept\" <collect@evil.example>"),
        )
        .expect("flagged");
        assert_eq!(warning.tier, SafetyTier::Danger);
        assert_eq!(
            warning.details,
            vec![
                ("From", "support@paypa1.com".to_owned()),
                ("Sender's domain", "paypa1.com".to_owned()),
                ("Looks like", "paypal.com".to_owned()),
                (
                    "Replies go to (Reply-To)",
                    "collect@evil.example".to_owned()
                ),
            ]
        );
    }
}
