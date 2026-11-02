//! The reader's banners on each message: the sender-safety banner and the
//! remote-content notice.
//!
//! Remote content (ADR 0006, Rob's decision) is blocked by default. A
//! message that references any shows a neutral notice with "Load remote
//! content" (for that message, until Zmail quits) and "Always for this
//! sender" (stored per account and sender address); an always-allowed
//! sender's messages say so and offer "Block". A message whose sender
//! warrants a Caution or Danger warning (whether or not the user hid it)
//! never offers "Always for this sender": only this message's content
//! can be loaded. The text view itself never
//! loads anything: the choice is what HTML renderers are given.
//!
//! Sender safety comes from `zmail_security::sender_safety` (#108), and
//! what the banner says from `zmail_security::sender_warning`, which the
//! terminal UI shows too. Each message gets at most one banner, at the
//! highest tier that applies:
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
use zmail_core::{ThreadMessage, participants::sender_identity, preferences::SenderWarnings};
use zmail_security::{
    sender_safety::SafetyTier,
    sender_warning::{sender_is_suspect, sender_warning},
};

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
    let offers_always = !sender_is_suspect(message);
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
        .when(offers_always, |banner| banner.action(always())),
        RemoteContentState::AllowedForMessage => Banner::new(
            ("remote-content", id),
            "Remote content is allowed in this message until Zmail quits.",
        )
        .when(offers_always, |banner| banner.action(always())),
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
