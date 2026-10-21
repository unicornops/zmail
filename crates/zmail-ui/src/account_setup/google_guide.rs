//! The Gmail sign-in step's alternative to an app password: a checklist
//! for creating a Google OAuth client of your own, with links into the
//! Google Cloud console, ending in the fields to paste it into. The steps
//! themselves are in [`crate::google_oauth`].

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, FontWeight, IntoElement, Role, SharedString, div,
    prelude::*, px, rgb,
};

use super::render::{caption, field, heading};
use super::{AccountField, AccountSetupView};
use crate::a11y::AccessibleExt;
use crate::components::{Banner, BannerTone, Button, ButtonSize};
use crate::google_oauth::{self, GUIDE_STEPS, GuideStep};
use crate::styles::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TYPE_BODY_STRONG, TypeStyleExt, UiColors};

/// The diameter of a step's number.
const STEP_NUMBER_SIZE: f32 = 24.0;

impl AccountSetupView {
    pub(super) fn render_google_guide(&self, colors: &UiColors) -> AnyElement {
        let (last, steps) = GUIDE_STEPS.split_last().expect("the guide has steps");
        let rows = steps
            .iter()
            .enumerate()
            .map(|(index, step)| guide_step(index + 1, step, colors).into_any_element());
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Use your own Google OAuth client",
                "An app password is quicker. If you can't use one, create a free OAuth client in Google Cloud: about ten minutes, once. zmail never sees your Google password.",
                colors,
            ))
            .child(
                div().flex().debug_selector(|| "account-setup-google-guide-docs".into()).child(
                    Button::new("account-setup-google-guide-docs", "Read the full guide")
                        .ghost()
                        .size(ButtonSize::Small)
                        .icon("external-link")
                        .on_click(|_, _, cx: &mut App| cx.open_url(google_oauth::GUIDE_URL)),
                ),
            )
            .child(
                div()
                    .id("account-setup-google-steps")
                    .role(Role::List)
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_3))
                    .children(rows),
            )
            .child(
                Banner::new("account-setup-google-testing", google_oauth::TESTING_EXPIRY)
                    .tone(BannerTone::Caution)
                    .title("Sign-ins expire after 7 days in Testing"),
            )
            .child(
                guide_step(GUIDE_STEPS.len(), last, colors)
                    .child(field(self, AccountField::Email, colors))
                    .child(field(self, AccountField::OauthClientId, colors))
                    .child(field(self, AccountField::OauthClientSecret, colors)),
            )
            .into_any_element()
    }
}

/// One numbered step: its title, what to do, and the console page it
/// happens on. The scope step can copy the scope.
fn guide_step(number: usize, step: &GuideStep, colors: &UiColors) -> gpui::Stateful<gpui::Div> {
    let id = step.id;
    let link = step.link.map(|(label, url)| {
        div()
            .debug_selector(move || format!("account-setup-google-step-{id}-open"))
            .child(
                Button::new(
                    SharedString::from(format!("account-setup-google-step-{id}-open")),
                    label,
                )
                .size(ButtonSize::Small)
                .icon("external-link")
                .on_click(move |_, _, cx: &mut App| cx.open_url(url)),
            )
    });
    let copy_scope = (id == "scopes").then(|| {
        div()
            .debug_selector(|| "account-setup-google-copy-scope".into())
            .child(
                Button::new("account-setup-google-copy-scope", "Copy scope")
                    .size(ButtonSize::Small)
                    .ghost()
                    .icon("copy")
                    .tooltip(google_oauth::MAIL_SCOPE)
                    .on_click(|_: &ClickEvent, _, cx: &mut App| {
                        cx.write_to_clipboard(ClipboardItem::new_string(
                            google_oauth::MAIL_SCOPE.to_owned(),
                        ))
                    }),
            )
    });
    div()
        .id(SharedString::from(format!(
            "account-setup-google-step-{id}"
        )))
        .debug_selector(move || format!("account-setup-google-step-{id}"))
        .accessible(Role::ListItem, format!("Step {number}: {}", step.title))
        .flex()
        .flex_col()
        .gap(px(SPACE_3))
        .p(px(SPACE_4))
        .rounded_lg()
        .bg(rgb(colors.surface))
        .border_1()
        .border_color(rgb(colors.border))
        .child(
            div()
                .flex()
                .gap(px(SPACE_3))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(STEP_NUMBER_SIZE))
                        .rounded_full()
                        .bg(rgb(colors.button_background))
                        .text_color(rgb(colors.accent))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_sm()
                        .child(number.to_string()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(SPACE_1))
                        .child(div().type_style(TYPE_BODY_STRONG).child(step.title))
                        .child(caption(step.body, colors)),
                ),
        )
        .when(link.is_some() || copy_scope.is_some(), |this| {
            this.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_2))
                    .pl(px(STEP_NUMBER_SIZE + SPACE_3))
                    .children(link)
                    .children(copy_scope),
            )
        })
}
