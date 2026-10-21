//! An expanded message's header: avatar, sender, a recipient summary
//! ("to me, Alice +2"), the date, and the message's Reply and More
//! buttons.
//!
//! - The sender's name shows the address in a tooltip; clicking it opens a
//!   menu that copies the address.
//! - Clicking the recipient summary shows the full From, To and Date
//!   lines below the header ([`render_details`]).
//! - The date is relative ("10:42", "Yesterday") with the full date in a
//!   tooltip.
//!
//! A click anywhere else on the header collapses the message.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, Role, SharedString, div, prelude::*, px, rgb,
};
use zmail_core::{
    ThreadMessage,
    participants::{Participant, parse_address_list, sender_identity},
    time::{format_full_date, format_list_date},
};

use super::ReaderView;
use super::message::{MessageContext, MessageDisplay};
use super::popover::PopoverAnchor;
use crate::a11y::{AccessibleExt, readable};
use crate::actions::ReplyToMessage;
use crate::components::{Avatar, ButtonSize, HitTargetExt, IconButton, MIN_TARGET_SIZE};
use crate::styles::{ICON_SIZE_SM, UiColors, rems_from_px};
use crate::tooltip::text_tooltip;
use crate::widgets::{EllipsisExt, icon};

/// How many recipients the summary names before "+N".
const NAMED_RECIPIENTS: usize = 2;

/// The header of an expanded message.
pub(super) fn render_header(
    reader: &ReaderView,
    message: &ThreadMessage,
    display: MessageDisplay,
    context: &MessageContext<'_>,
    cx: &mut Context<ReaderView>,
) -> AnyElement {
    let colors = &context.colors;
    let (sender_name, sender_email) = sender_identity(&message.sender);
    let date = format_list_date(&message.received_at, &context.now, context.hour_cycle);
    let full_date = format_full_date(
        &message.received_at,
        &context.now.timezone(),
        context.hour_cycle,
    );
    let summary = recipient_summary(&message.recipients, context.own_addresses);
    let index = display.index;
    let message_id = message.id;
    let focus_handle = reader.focus_handle.clone();

    let mut avatar = Avatar::new(sender_name.clone());
    if let Some(email) = &sender_email {
        avatar = avatar.address(email.clone());
    }
    let sender_label = match &sender_email {
        Some(email) if *email != sender_name => format!("{sender_name}, {email}"),
        _ => sender_name.clone(),
    };
    let sender = div()
        .id(("reader-sender", message.id as u64))
        .hit_target(move || format!("reader-sender-{index}"))
        .accessible(Role::Button, sender_label)
        .aria_description("Sender details")
        .aria_expanded(reader.popover_anchor_is(PopoverAnchor::Sender(index)))
        .max_w_full()
        .ellipsis()
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|style| style.underline())
        .when_some(sender_email.clone(), |this, email| {
            this.tooltip(text_tooltip(email, *colors))
        })
        .on_click(cx.listener(move |reader, _, window, cx| {
            cx.stop_propagation();
            reader.toggle_popover(PopoverAnchor::Sender(index), window, cx);
        }))
        .child(sender_name);

    div()
        .id(("reader-message-header", message.id as u64))
        .hit_target(move || format!("reader-message-header-{index}"))
        .flex()
        .items_start()
        .gap_3()
        .px_4()
        .pt_4()
        .pb_3()
        .cursor_pointer()
        .on_click(cx.listener(move |reader, _, _, cx| reader.toggle_message(index, cx)))
        .child(avatar)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().flex().min_w_0().child(reader.popover_trigger(
                    PopoverAnchor::Sender(index),
                    sender,
                    cx,
                )))
                .when(!summary.is_empty(), |this| {
                    this.child(
                        div().flex().min_w_0().child(
                            div()
                                .id(("reader-recipients", message.id as u64))
                                .hit_target(move || format!("reader-recipients-{index}"))
                                .accessible(Role::Button, summary.clone())
                                .aria_description("From, To and Date details")
                                .aria_expanded(display.details)
                                .flex()
                                .items_center()
                                .min_h(px(MIN_TARGET_SIZE))
                                .gap_1()
                                .min_w_0()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .cursor_pointer()
                                .hover(|style| style.text_color(rgb(colors.foreground)))
                                .tooltip(text_tooltip(
                                    if display.details {
                                        "Hide details"
                                    } else {
                                        "Show details"
                                    },
                                    *colors,
                                ))
                                .on_click(cx.listener(move |reader, _, _, cx| {
                                    cx.stop_propagation();
                                    reader.toggle_details(index, cx);
                                }))
                                .child(div().min_w_0().ellipsis().child(summary))
                                .child(icon(
                                    if display.details {
                                        "chevron-down"
                                    } else {
                                        "chevron-right"
                                    },
                                    ICON_SIZE_SM,
                                    colors.muted,
                                )),
                        ),
                    )
                }),
        )
        .child(
            div()
                .id(("reader-message-date", message.id as u64))
                .flex_none()
                .pt_0p5()
                .whitespace_nowrap()
                .text_sm()
                .text_color(rgb(colors.muted))
                .tooltip(text_tooltip(full_date, *colors))
                .child(date),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_0p5()
                .child(
                    div()
                        .debug_selector(move || format!("reader-reply-{index}"))
                        .child(
                            IconButton::new(("reader-reply", message.id as u64), "reply", "Reply")
                                .size(ButtonSize::Small)
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    focus_handle.dispatch_action(
                                        &ReplyToMessage { message_id },
                                        window,
                                        cx,
                                    );
                                }),
                        ),
                )
                .child(
                    div()
                        .debug_selector(move || format!("reader-message-more-{index}"))
                        .child(
                            reader.popover_trigger(
                                PopoverAnchor::MessageMore(index),
                                IconButton::new(
                                    ("reader-message-more", message.id as u64),
                                    "ellipsis",
                                    "More",
                                )
                                .size(ButtonSize::Small)
                                .selected(
                                    reader.popover_anchor_is(PopoverAnchor::MessageMore(index)),
                                )
                                .on_click(cx.listener(
                                    move |reader, _, window, cx| {
                                        cx.stop_propagation();
                                        reader.toggle_popover(
                                            PopoverAnchor::MessageMore(index),
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                                cx,
                            ),
                        ),
                ),
        )
        .into_any_element()
}

/// The From, To and Date lines the recipient summary expands to.
pub(super) fn render_details(message: &ThreadMessage, context: &MessageContext<'_>) -> AnyElement {
    let colors = &context.colors;
    let full_date = format_full_date(
        &message.received_at,
        &context.now.timezone(),
        context.hour_cycle,
    );
    let to = parse_address_list(&message.recipients)
        .iter()
        .map(full_address)
        .collect::<Vec<_>>()
        .join(", ");
    div()
        .flex()
        .flex_col()
        .gap_1()
        .text_sm()
        .child(detail_line("From", message.sender.trim(), colors))
        .when(!to.is_empty(), |this| {
            this.child(detail_line("To", &to, colors))
        })
        .child(detail_line("Date", &full_date, colors))
        .into_any_element()
}

fn detail_line(label: &str, value: &str, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .gap_2()
        .min_w_0()
        .child(
            div()
                .flex_none()
                .w(rems_from_px(40.))
                .text_color(rgb(colors.muted))
                .child(readable(
                    SharedString::from(format!("detail-label-{label}")),
                    label.to_owned(),
                )),
        )
        .child(div().flex_1().min_w_0().child(readable(
            SharedString::from(format!("detail-value-{label}")),
            value.to_owned(),
        )))
}

/// "Name <address>", or whichever of the two the participant has.
fn full_address(participant: &Participant) -> String {
    match (&participant.name, &participant.email) {
        (Some(name), Some(email)) if !name.trim().is_empty() => format!("{name} <{email}>"),
        _ => participant.display_name().to_owned(),
    }
}

/// The recipients in a few words: "to me, Alice +2". The user's own
/// addresses read "me" and come first.
pub(super) fn recipient_summary(recipients: &str, own_addresses: &[String]) -> String {
    let mut me = false;
    let mut others = Vec::new();
    for participant in parse_address_list(recipients) {
        let is_me = participant.email.as_deref().is_some_and(|email| {
            own_addresses
                .iter()
                .any(|own| own.eq_ignore_ascii_case(email.trim()))
        });
        if is_me {
            me = true;
        } else {
            others.push(participant.display_name().to_owned());
        }
    }
    let names: Vec<String> = me
        .then(|| "me".to_owned())
        .into_iter()
        .chain(others)
        .collect();
    if names.is_empty() {
        return String::new();
    }
    let shown = names.len().min(NAMED_RECIPIENTS);
    let mut summary = format!("to {}", names[..shown].join(", "));
    if names.len() > shown {
        summary.push_str(&format!(" +{}", names.len() - shown));
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::recipient_summary;

    #[test]
    fn the_summary_names_me_first_then_counts_the_rest() {
        let own = ["sam@example.com".to_owned()];
        assert_eq!(
            recipient_summary(
                "Alice <alice@example.com>, Sam <SAM@example.com>, Bob <bob@example.com>, carol@example.com",
                &own
            ),
            "to me, Alice +2"
        );
        assert_eq!(
            recipient_summary("Alice <alice@example.com>", &own),
            "to Alice"
        );
        assert_eq!(
            recipient_summary("Alice <alice@example.com>, Bob <bob@example.com>", &own),
            "to Alice, Bob"
        );
        assert_eq!(recipient_summary("sam@example.com", &own), "to me");
        assert_eq!(recipient_summary("", &own), "");
    }
}
