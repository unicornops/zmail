//! Settings → Privacy: what Zmail does to protect you, the senders whose
//! remote content you always load, and how many sender warnings to show.
//!
//! Remote content is blocked until the user allows it on a message
//! (ADR 0006): "Load remote content" for that message until Zmail quits,
//! or "Always for this sender", stored per account and address. The row
//! says so, and below it the always-allowed senders are listed from the
//! store, each with Remove, which blocks that sender's remote content
//! again (as "Block" on one of their messages does).
//!
//! Sender warnings have two levels (`UiPreferences::sender_warnings`):
//! Standard shows every warning; Fewer warnings leaves out the
//! informational one for a Reply-To on another part of the sender's own
//! domain. Warnings about lookalike senders and unrelated Reply-To domains
//! always show.

use gpui::{
    AnyElement, AppContext as _, Context, Entity, IntoElement, Render, Role, SharedString,
    Subscription, Window, div, prelude::*, px, rgb,
};
use zmail_core::{AccountColor, preferences::SenderWarnings};
use zmail_store::RemoteContentSender;

use super::{
    SettingsEvent, SettingsHost, apply_to_workspace, dotted_label, group_heading, section_heading,
    setting_row, update_workspace,
};
use crate::a11y::AccessibleExt;
use crate::components::{
    Badge, BadgeTone, Button, ButtonSize, ComponentTheme, Select, SelectEvent,
};
use crate::styles::{SPACE_1, SPACE_2, SPACE_3, TYPE_BODY, TYPE_CAPTION, TypeStyleExt, UiColors};
use crate::widgets::EllipsisExt;

/// The sender-warning levels, in picker order.
const SENDER_WARNINGS: [(SenderWarnings, &str); 2] = [
    (SenderWarnings::Standard, "Standard"),
    (SenderWarnings::Fewer, "Fewer Warnings"),
];

/// The remote-content row's badge.
pub(crate) const REMOTE_CONTENT_BADGE: &str = "Blocked until you allow it";
/// The heading over the always-allowed senders.
pub(crate) const ALLOWED_SENDERS_HEADING: &str = "Senders you always load";
/// What the list is, under its heading.
const ALLOWED_SENDERS_NOTE: &str = "Remote content in mail from these addresses loads without asking. Remove one to block it again.";
/// The list's empty state.
pub(crate) const NO_ALLOWED_SENDERS: &str =
    "None yet. Choose Always for this sender on a message's remote-content notice to add one.";

/// An always-allowed sender as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AllowedSender {
    pub(crate) account_id: i64,
    pub(crate) address: String,
    /// The account's name, when there is more than one account to tell
    /// apart.
    account_name: Option<String>,
    color: AccountColor,
}

/// The Privacy section.
pub(crate) struct PrivacySettings {
    sender_warnings: Entity<Select>,
    allowed_senders: Vec<AllowedSender>,
    _subscription: Subscription,
}

impl PrivacySettings {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let sender_warnings = cx.new(|cx| {
            Select::new(SENDER_WARNINGS.map(|(_, label)| label), None, cx)
                .label("Sender warnings")
                .width(px(200.0))
        });
        let subscription = cx.subscribe(&sender_warnings, |_, _, event: &SelectEvent, cx| {
            let SelectEvent::Changed(index) = *event;
            let level = SENDER_WARNINGS[index].0;
            match SettingsHost::get(cx)
                .store
                .update_ui_preferences(|preferences| preferences.sender_warnings = level)
            {
                Ok(_) => apply_to_workspace(cx),
                Err(error) => cx.emit(SettingsEvent::Error(format!(
                    "Could not save the sender warnings: {error:#}"
                ))),
            }
        });
        let mut this = Self {
            sender_warnings,
            allowed_senders: Vec::new(),
            _subscription: subscription,
        };
        this.reload(cx);
        this
    }

    /// Show the saved level and the always-allowed senders.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let level = SettingsHost::get(cx)
            .store
            .ui_preferences()
            .unwrap_or_default()
            .sender_warnings;
        self.sender_warnings.update(cx, |select, cx| {
            select.set_selected(
                SENDER_WARNINGS
                    .iter()
                    .position(|(candidate, _)| *candidate == level),
                cx,
            )
        });
        self.refresh(cx);
    }

    /// Re-read the always-allowed senders: the main window adds them.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        let store = &SettingsHost::get(cx).store;
        let senders = store.list_remote_content_senders();
        let accounts = store.list_accounts();
        let (senders, accounts) = match (senders, accounts) {
            (Ok(senders), Ok(accounts)) => (senders, accounts),
            (Err(error), _) | (_, Err(error)) => {
                cx.emit(SettingsEvent::Error(format!(
                    "Could not load the senders you always load: {error:#}"
                )));
                return;
            }
        };
        let several_accounts = accounts.len() > 1;
        let allowed_senders = senders
            .into_iter()
            .filter_map(
                |RemoteContentSender {
                     account_id,
                     address,
                 }| {
                    let account = accounts.iter().find(|account| account.id == account_id)?;
                    Some(AllowedSender {
                        account_id,
                        address,
                        account_name: several_accounts.then(|| account.display_name.clone()),
                        color: account.color(),
                    })
                },
            )
            .collect::<Vec<_>>();
        if allowed_senders != self.allowed_senders {
            self.allowed_senders = allowed_senders;
            cx.notify();
        }
    }

    /// Remove: block `address`'s remote content on `account_id` again.
    pub(crate) fn remove_sender(&mut self, account_id: i64, address: &str, cx: &mut Context<Self>) {
        match SettingsHost::get(cx)
            .store
            .remove_remote_content_sender(account_id, address)
        {
            Ok(_) => {
                // The reader shows the sender's messages blocked again.
                update_workspace(cx, |view, _, cx| {
                    view.refresh_from_store(cx);
                    cx.notify();
                });
                cx.emit(SettingsEvent::Status(format!(
                    "Remote content from {address} is blocked again."
                )));
            }
            Err(error) => cx.emit(SettingsEvent::Error(format!(
                "Could not remove {address}: {error:#}"
            ))),
        }
        self.refresh(cx);
    }

    /// The sender-warnings picker, for tests to drive.
    #[cfg(test)]
    pub(crate) fn sender_warnings(&self) -> &Entity<Select> {
        &self.sender_warnings
    }

    /// The always-allowed senders listed, for tests.
    #[cfg(test)]
    pub(crate) fn allowed_senders(&self) -> &[AllowedSender] {
        &self.allowed_senders
    }

    fn render_allowed_senders(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let rows = self
            .allowed_senders
            .iter()
            .enumerate()
            .map(|(index, sender)| {
                let account_id = sender.account_id;
                let address = sender.address.clone();
                let spoken = match &sender.account_name {
                    Some(account) => format!("{address}, on {account}"),
                    None => address.clone(),
                };
                let line = div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        div()
                            .type_style(TYPE_BODY)
                            .ellipsis()
                            .child(sender.address.clone()),
                    )
                    .when_some(sender.account_name.clone(), |this, account| {
                        this.child(
                            div()
                                .type_style(TYPE_CAPTION)
                                .text_color(rgb(colors.muted))
                                .ellipsis()
                                .child(account),
                        )
                    });
                let selector = format!("allowed-sender-remove-{account_id}-{address}");
                div()
                    .id(("allowed-sender", index))
                    .accessible(Role::ListItem, spoken.clone())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_2))
                    .py(px(SPACE_1))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(dotted_label(sender.color, line)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .debug_selector(move || selector.clone())
                            .child(
                                Button::new(("allowed-sender-remove", index), "Remove")
                                    .size(ButtonSize::Small)
                                    .tooltip(SharedString::from(format!(
                                        "Block remote content from {spoken} again"
                                    )))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_sender(account_id, &address, cx)
                                    })),
                            ),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let list = if rows.is_empty() {
            div()
                .debug_selector(|| "allowed-senders-empty".to_owned())
                .pt(px(SPACE_2))
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .child(NO_ALLOWED_SENDERS)
                .into_any_element()
        } else {
            div()
                .id("allowed-senders")
                .accessible(Role::List, ALLOWED_SENDERS_HEADING.to_owned())
                .flex()
                .flex_col()
                .pt(px(SPACE_2))
                .children(rows)
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .pb(px(SPACE_3))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .type_style(TYPE_CAPTION)
                    .text_color(rgb(colors.muted))
                    .child(ALLOWED_SENDERS_NOTE),
            )
            .child(list)
            .into_any_element()
    }
}

impl Render for PrivacySettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        div()
            .flex()
            .flex_col()
            .child(section_heading("Privacy"))
            .child(setting_row(
                "Remote content",
                Some(
                    "Images and other files a message loads from the web can tell the sender when and where you read it. Zmail blocks them and says so above the message. Load remote content shows them in that message until Zmail quits; Always for this sender loads them in every message from that address."
                        .into(),
                ),
                Badge::label(REMOTE_CONTENT_BADGE).tone(BadgeTone::Neutral),
                &colors,
            ))
            .child(setting_row(
                "Sender warnings",
                Some(
                    "A banner warns when a sender's domain imitates a well-known one, or when replies would go to a different domain. Fewer Warnings leaves out the note for replies that go to another part of the sender's own domain; the other warnings always show."
                        .into(),
                ),
                self.sender_warnings.clone(),
                &colors,
            ))
            .child(group_heading(ALLOWED_SENDERS_HEADING, &colors))
            .child(self.render_allowed_senders(&colors, cx))
            .child(group_heading("Links and attachments", &colors))
            .child(setting_row(
                "Before opening",
                Some(
                    "A link opens only after a dialog shows where it really goes, and warns when its text names a different site. Zmail also asks before opening a program, a script or a web page from an attachment."
                        .into(),
                ),
                Badge::label("Always").tone(BadgeTone::Neutral),
                &colors,
            ))
    }
}
