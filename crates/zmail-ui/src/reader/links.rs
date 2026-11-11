//! Links in a message body, and the dialog that opens one.
//!
//! A body's links are the web and `mailto:` addresses written in its text,
//! plus, for an HTML message, the text of its `<a href>` links, which the
//! store reads with their original targets (scheme included) from the
//! sanitiser's link table ([`zmail_core::BodyLink`]). Nothing is loaded: a
//! link opens only when the user clicks it and then confirms in a dialog
//! that shows the real target. When the link's text shows a web address
//! on a different site from the target (the usual phishing trick), the
//! dialog says so. Which targets open, the addresses written in text and
//! that check are `zmail_security::links`, shared with the terminal UI; a
//! `mailto:` link keeps only its `to`, `cc`, `bcc`, `subject` and `body`
//! fields.

use std::ops::Range;

use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, IntoElement, SharedString, Window, div,
    prelude::*, px, rgb,
};
use url::Url;
pub(crate) use zmail_security::links::link_target;
use zmail_security::links::{misleading_host, written_urls};

use super::ReaderView;
use crate::a11y::readable;
use crate::components::{Button, Modal};
use crate::styles::{ICON_SIZE_SM, UiColors};
use crate::widgets::icon;

/// A link in a message body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BodyLink {
    /// The link's text, as a byte range of the body.
    pub(crate) range: Range<usize>,
    /// Where it goes: an `http`, `https` or `mailto` URL.
    pub(crate) target: String,
}

/// The links of a body, in order: `html_links`, the links the store read
/// from the message's HTML, then the addresses written in the text.
pub(super) fn body_links(body: &str, html_links: &[zmail_core::BodyLink]) -> Vec<BodyLink> {
    let mut links: Vec<BodyLink> = html_links
        .iter()
        .filter(|link| {
            link.range.start < link.range.end
                && body.is_char_boundary(link.range.start)
                && body.is_char_boundary(link.range.end)
        })
        .filter_map(|link| {
            Some(BodyLink {
                range: link.range.clone(),
                target: link_target(&link.target)?,
            })
        })
        .collect();
    for range in written_urls(body) {
        if links.iter().any(|link| overlaps(&link.range, &range)) {
            continue;
        }
        if let Some(target) = link_target(&body[range.clone()]) {
            links.push(BodyLink { range, target });
        }
    }
    links.sort_by_key(|link| link.range.start);
    links
}

fn overlaps(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

/// What the open-link dialog shows about a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkPrompt {
    /// The full target URL.
    pub(crate) target: String,
    /// The target's host (`None` for `mailto:`).
    pub(crate) host: Option<String>,
    /// The site the link's text shows, when it differs from the target's.
    pub(crate) shown_host: Option<String>,
}

impl LinkPrompt {
    /// The dialog for a link whose text is `text` and target `target`.
    pub(crate) fn new(text: &str, target: &str) -> Self {
        let host = Url::parse(target)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned));
        let shown_host = misleading_host(text, target);
        Self {
            target: target.to_owned(),
            host,
            shown_host,
        }
    }

    /// Whether the link's text shows a different site from its target.
    pub(crate) fn is_mismatched(&self) -> bool {
        self.shown_host.is_some()
    }
}

impl ReaderView {
    /// A click on link `link` of the message `message_id`: shows the
    /// open-link dialog, unless the click was the end of a text selection.
    pub(super) fn link_clicked(
        &mut self,
        message_id: i64,
        link: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_selection() {
            return;
        }
        let Some(body) = self.bodies.get(&message_id) else {
            return;
        };
        let Some(link) = body.links.get(link) else {
            return;
        };
        let text = &body.text[link.range.clone()];
        self.link_prompt = Some(LinkPrompt::new(text, &link.target));
        self.link_modal.open(window, cx);
        cx.notify();
    }

    /// The open-link dialog's link.
    #[cfg(test)]
    pub(crate) fn link_prompt(&self) -> Option<&LinkPrompt> {
        self.link_prompt
            .as_ref()
            .filter(|_| self.link_modal.is_open())
    }

    fn close_link_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.link_prompt = None;
        self.link_modal.close(window, cx);
        cx.notify();
    }

    /// Opens the dialog's link in the system browser (or mail app) and
    /// closes the dialog.
    fn open_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(prompt) = self.link_prompt.as_ref() {
            cx.open_url(&prompt.target);
        }
        self.close_link_prompt(window, cx);
    }

    /// The open-link dialog, while it is open.
    pub(super) fn render_link_prompt(
        &self,
        colors: &UiColors,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let prompt = self
            .link_prompt
            .as_ref()
            .filter(|_| self.link_modal.is_open())?;
        let mailto = prompt.host.is_none();
        let target = prompt.target.clone();
        let warning = prompt.shown_host.as_ref().map(|shown| {
            let goes_to = prompt
                .host
                .as_deref()
                .map_or_else(|| "an email address".to_owned(), str::to_owned);
            div()
                .debug_selector(|| "reader-link-warning".into())
                .flex()
                .items_start()
                .gap_2()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(rgb(colors.warning))
                .bg(rgb(colors.warning_background))
                .child(icon("shield-alert", ICON_SIZE_SM, colors.warning))
                .child(
                    // Takes the rest of the row and wraps there.
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_sm()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("This link is not what it shows"),
                        )
                        .child(format!(
                            "The link's text shows {shown}, but it goes to {goes_to}."
                        )),
                )
        });
        let open_button = Button::new(
            "reader-link-open",
            if mailto { "Write Email" } else { "Open Link" },
        )
        .on_click(cx.listener(|reader, _, window, cx| reader.open_link(window, cx)));
        // A mismatched link makes Cancel the main button.
        let (open_button, cancel_button) = if prompt.is_mismatched() {
            (
                open_button.danger(),
                Button::new("reader-link-cancel", "Cancel").primary(),
            )
        } else {
            (
                open_button.primary(),
                Button::new("reader-link-cancel", "Cancel"),
            )
        };
        let title: SharedString = if mailto {
            "Write to this address?".into()
        } else {
            "Open this link?".into()
        };
        let description: SharedString = match &prompt.host {
            Some(host) => format!("It goes to {host} and opens in your browser.").into(),
            None => "It opens in your email app.".into(),
        };
        Some(
            Modal::new("reader-link", self.link_modal.focus_handle())
                .title(title)
                .description(description)
                .width(px(520.))
                .children(warning)
                .child(
                    div()
                        .debug_selector(|| "reader-link-target".into())
                        .p_3()
                        .rounded_md()
                        .bg(rgb(colors.surface))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .text_sm()
                        // The whole address, read out before the choice.
                        .child(readable("reader-link-target", prompt.target.clone())),
                )
                .footer(
                    Button::new("reader-link-copy", "Copy Link")
                        .icon("copy")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(target.clone()));
                        }),
                )
                .footer(cancel_button.on_click(
                    cx.listener(|reader, _, window, cx| reader.close_link_prompt(window, cx)),
                ))
                .footer(
                    div()
                        .debug_selector(|| "reader-link-open".into())
                        .child(open_button),
                )
                .on_dismiss(
                    cx.listener(|reader, _, window, cx| reader.close_link_prompt(window, cx)),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{BodyLink, LinkPrompt, body_links, link_target};

    fn texts(body: &str, html_links: &[zmail_core::BodyLink]) -> Vec<(String, String)> {
        body_links(body, html_links)
            .into_iter()
            .map(|BodyLink { range, target }| (body[range].to_owned(), target))
            .collect()
    }

    #[test]
    fn written_addresses_are_links_without_trailing_punctuation() {
        let body = "See https://example.com/changelog. Or (http://example.org/a_(b)), \
                    mailto:help@example.com, and xhttps://no.example.";
        assert_eq!(
            texts(body, &[]),
            [
                (
                    "https://example.com/changelog".to_owned(),
                    "https://example.com/changelog".to_owned()
                ),
                (
                    "http://example.org/a_(b)".to_owned(),
                    "http://example.org/a_(b)".to_owned()
                ),
                (
                    "mailto:help@example.com".to_owned(),
                    "mailto:help@example.com".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn html_links_point_at_their_real_target() {
        // As the store reads them from the sanitiser's link table.
        let body = "Pay your https://bank.example bill.\nCall Mail";
        let html_link = |text: &str, target: &str| {
            let start = body.find(text).expect("the link text is in the body");
            zmail_core::BodyLink {
                range: start..start + text.len(),
                target: target.to_owned(),
            }
        };
        assert_eq!(
            texts(
                body,
                &[
                    html_link("https://bank.example", "http://evil.example/pay"),
                    html_link("Call", "tel:+3531234567"),
                    html_link("Mail", "mailto:x@example.com"),
                ]
            ),
            [
                (
                    "https://bank.example".to_owned(),
                    "http://evil.example/pay".to_owned()
                ),
                ("Mail".to_owned(), "mailto:x@example.com".to_owned()),
            ]
        );
    }

    #[test]
    fn only_web_and_mail_targets_open() {
        assert_eq!(
            link_target("http://example.org/x").as_deref(),
            Some("http://example.org/x")
        );
        for href in [
            "blocked://example.org/x",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "#top",
            "/relative",
        ] {
            assert_eq!(link_target(href), None, "{href}");
        }
    }

    #[test]
    fn a_link_showing_another_site_is_mismatched() {
        let prompt = LinkPrompt::new("https://bank.example/login", "https://evil.example/x");
        assert_eq!(prompt.host.as_deref(), Some("evil.example"));
        assert_eq!(prompt.shown_host.as_deref(), Some("bank.example"));

        // The userinfo trick: the host is what follows the @.
        let prompt = LinkPrompt::new("bank.example", "https://bank.example@evil.example/");
        assert!(prompt.is_mismatched());
        assert_eq!(prompt.host.as_deref(), Some("evil.example"));

        for (text, target) in [
            ("Pay your bill", "https://evil.example/x"),
            ("www.bank.example", "https://bank.example/login"),
            (
                "https://example.com/changelog",
                "https://example.com/changelog",
            ),
            ("help@example.com", "mailto:help@example.com"),
            ("v1.2", "https://example.com/"),
        ] {
            assert!(!LinkPrompt::new(text, target).is_mismatched(), "{text}");
        }

        // Disguised link text is still read.
        for text in [
            "Sign in at https://bank.example",
            "bank.example/@login",
            "bank\u{200B}.example",
            "bank.example.:443",
        ] {
            let prompt = LinkPrompt::new(text, "https://evil.example/");
            assert_eq!(
                prompt.shown_host.as_deref(),
                Some("bank.example"),
                "{text:?}"
            );
        }
    }
}
