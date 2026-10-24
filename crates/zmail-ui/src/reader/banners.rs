//! The reader's banners on each message: the sender-safety banner and the
//! remote-content notice.
//!
//! Remote content (ADR 0006, Rob's decision) is blocked by default. A
//! message that references any shows a neutral notice with "Load remote
//! content" (for that message, until Zmail quits) and "Always for this
//! sender" (stored per account and sender address); an always-allowed
//! sender's messages say so and offer "Block". The text view itself never
//! loads anything: the choice is what HTML renderers are given.
//!
//! Sender safety comes from `zmail_security::sender_safety` (#108). Each
//! message gets at most one banner, at the highest tier that applies:
//!
//! | Finding | Tier |
//! |---|---|
//! | The From domain imitates a well-known domain | Danger |
//! | Reply-To is on an unrelated domain | Caution |
//! | Reply-To is on a parent or subdomain of the From domain | Info |
//!
//! "Looks safe" hides the banner for that message only (stored with the
//! message); "Show details" shows the evidence the banner is based on.
//! With Settings → Privacy → Sender warnings on "Fewer warnings", Info
//! banners do not show; Caution and Danger always do.

use gpui::{AnyElement, IntoElement, SharedString, div, prelude::*};
use zmail_core::{
    ThreadMessage,
    participants::{parse_address_list, sender_identity},
    preferences::SenderWarnings,
};
use zmail_security::sender_safety::{SafetyTier, SenderSafetyReport, analyze_sender_identity};

use crate::actions::{
    AlwaysLoadRemoteContent, BlockRemoteContent, DismissSenderWarning, LoadRemoteContent,
};
use crate::components::{Banner, BannerTone, Button, ButtonSize};

/// The banners of `message`: its sender-safety banner, unless its sender
/// looks safe, the user dismissed it, or `level` leaves its tier out; then
/// its remote-content notice, when it references remote content.
/// `remote_allowed` is "Load remote content" for this message.
pub(crate) fn message_banners(
    message: &ThreadMessage,
    level: SenderWarnings,
    remote_allowed: bool,
) -> Vec<AnyElement> {
    sender_safety_banner(message, level)
        .map(IntoElement::into_any_element)
        .into_iter()
        .chain(remote_content_banner(message, remote_allowed).map(IntoElement::into_any_element))
        .collect()
}

/// What a message's remote content is allowed to do, as its banner says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteContentState {
    /// Blocked: the default.
    Blocked,
    /// "Load remote content" for this message.
    AllowedForMessage,
    /// "Always for this sender".
    AllowedForSender,
}

/// The remote-content state of `message`, or `None` when it references
/// no remote content.
pub(crate) fn remote_content_state(
    message: &ThreadMessage,
    remote_allowed: bool,
) -> Option<RemoteContentState> {
    if !message.has_remote_content {
        return None;
    }
    Some(if message.remote_content_sender_allowed {
        RemoteContentState::AllowedForSender
    } else if remote_allowed {
        RemoteContentState::AllowedForMessage
    } else {
        RemoteContentState::Blocked
    })
}

fn remote_content_banner(message: &ThreadMessage, remote_allowed: bool) -> Option<Banner> {
    let state = remote_content_state(message, remote_allowed)?;
    let message_id = message.id;
    let id = message_id as u64;
    let always = || {
        div()
            .debug_selector(move || format!("remote-content-always-{message_id}"))
            .child(
                Button::new(("remote-content-always", id), "Always for this sender")
                    .size(ButtonSize::Small)
                    .tooltip(SharedString::from(
                        "Load remote content in every message from this sender",
                    ))
                    .on_click(move |_, window, cx| {
                        window
                            .dispatch_action(Box::new(AlwaysLoadRemoteContent { message_id }), cx);
                    }),
            )
    };
    let banner = match state {
        RemoteContentState::Blocked => Banner::new(
            ("remote-content", id),
            "Remote content is blocked in this message to protect your privacy.",
        )
        .action(
            div()
                .debug_selector(move || format!("remote-content-load-{message_id}"))
                .child(
                    Button::new(("remote-content-load", id), "Load remote content")
                        .icon("eye")
                        .size(ButtonSize::Small)
                        .tooltip(SharedString::from(
                            "Allow this message's remote images and styles until Zmail quits",
                        ))
                        .on_click(move |_, window, cx| {
                            window.dispatch_action(Box::new(LoadRemoteContent { message_id }), cx);
                        }),
                ),
        )
        .action(always()),
        RemoteContentState::AllowedForMessage => Banner::new(
            ("remote-content", id),
            "Remote content is allowed in this message until Zmail quits.",
        )
        .action(always()),
        RemoteContentState::AllowedForSender => {
            let address = sender_identity(&message.sender)
                .1
                .unwrap_or_else(|| sender_identity(&message.sender).0);
            Banner::new(
                ("remote-content", id),
                format!("Remote content from {address} is always allowed."),
            )
            .action(
                div()
                    .debug_selector(move || format!("remote-content-block-{message_id}"))
                    .child(
                        Button::new(("remote-content-block", id), "Block")
                            .icon("eye-off")
                            .size(ButtonSize::Small)
                            .tooltip(SharedString::from(
                                "Block remote content from this sender again",
                            ))
                            .on_click(move |_, window, cx| {
                                window.dispatch_action(
                                    Box::new(BlockRemoteContent { message_id }),
                                    cx,
                                );
                            }),
                    ),
            )
        }
    };
    Some(banner)
}

/// What a sender-safety banner says: its tier, title, sentence and the
/// evidence under "Show details".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SenderWarning {
    pub(crate) tier: SafetyTier,
    pub(crate) title: &'static str,
    pub(crate) message: String,
    pub(crate) details: Vec<(&'static str, String)>,
}

/// The warning for `message`, or `None` when there is nothing to show at
/// `level`.
pub(crate) fn sender_warning(
    message: &ThreadMessage,
    level: SenderWarnings,
) -> Option<SenderWarning> {
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
pub(crate) fn shows(tier: SafetyTier, level: SenderWarnings) -> bool {
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

    let mut details = vec![("From", from_address)];
    let (title, message) = match tier {
        SafetyTier::Danger => {
            let known = report.lookalike_of.clone().unwrap_or_default();
            details.push(("Sender's domain", from_domain));
            details.push(("Looks like", known.clone()));
            (
                "Possible impersonation",
                format!(
                    "The sender's domain imitates {known}, so this message may not be from them."
                ),
            )
        }
        SafetyTier::Caution => {
            let reply_domain = report.reply_to_mismatch.clone().unwrap_or_default();
            (
                "Replies go somewhere else",
                format!(
                    "Replying sends your message to {reply_domain}, not to the sender's domain."
                ),
            )
        }
        SafetyTier::Info => {
            let reply_domain = report.reply_to_mismatch.clone().unwrap_or_default();
            (
                "Replies go to a related address",
                format!(
                    "Replies go to {reply_domain}, a related domain, rather than {from_domain}."
                ),
            )
        }
    };
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

fn sender_safety_banner(message: &ThreadMessage, level: SenderWarnings) -> Option<Banner> {
    let warning = sender_warning(message, level)?;
    let message_id = message.id;
    let tone = match warning.tier {
        SafetyTier::Info => BannerTone::Info,
        SafetyTier::Caution => BannerTone::Caution,
        SafetyTier::Danger => BannerTone::Danger,
    };
    let mut banner = Banner::new(("sender-safety", message_id as u64), warning.message)
        .tone(tone)
        .title(warning.title)
        .action(
            div()
                .debug_selector(|| format!("sender-safety-dismiss-{message_id}"))
                .child(
                    Button::new(("sender-safety-dismiss", message_id as u64), "Looks safe")
                        .icon("check")
                        .size(ButtonSize::Small)
                        .tooltip(SharedString::from("Hide this warning for this message"))
                        .on_click(move |_, window, cx| {
                            window
                                .dispatch_action(Box::new(DismissSenderWarning { message_id }), cx);
                        }),
                ),
        );
    for (label, value) in warning.details {
        banner = banner.detail(label, value);
    }
    Some(banner)
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
}
