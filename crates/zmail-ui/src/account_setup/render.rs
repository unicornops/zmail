//! Drawing the add-account flow: one centred column per stage, with the
//! stage's actions in a footer.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ClickEvent, Context, FontWeight, IntoElement,
    Render, Role, SharedString, Transformation, Window, div, percentage, prelude::*, px, rgb,
};
use zmail_autodiscover::{
    AuthMethod, MailSettings, Provider, ServerSettings, Source, Step, StepOutcome,
};
use zmail_core::{AccountColor, ProtocolKind, SyncPhase};

use super::{AccountField, AccountSetupView, KEY_CONTEXT, Stage};
use crate::a11y::{A11yFlags, AccessibleExt, Politeness, readable};
use crate::components::{Banner, BannerTone, Button, ButtonSize, ComponentTheme, HitTargetExt};
use crate::styles::{
    EMPTY_STATE_ICON_FRAME_SIZE, ICON_SIZE_LG, ICON_SIZE_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4,
    SPACE_6, SPACE_8, TYPE_BODY, TYPE_BODY_STRONG, TYPE_CAPTION, TYPE_HEADLINE, TypeStyleExt,
    UiColors, rems_from_px,
};
use crate::widgets::{account_color, icon, input_card, section_card};

/// The width of the flow's column.
const COLUMN_WIDTH: f32 = 560.0;
/// The diameter of a colour swatch.
const SWATCH_SIZE: f32 = 36.0;
/// How long the spinner takes to turn once.
const SPIN_PERIOD: Duration = Duration::from_millis(1200);

/// Where to learn how to make an app password, for the providers that need
/// one.
fn app_password_help(provider: Provider) -> Option<(&'static str, &'static str)> {
    match provider {
        Provider::Google => Some((
            "Google needs an app password, not your Google password. Turn on 2-Step Verification, then create an app password for zmail.",
            "https://support.google.com/accounts/answer/185833",
        )),
        Provider::ICloud => Some((
            "iCloud needs an app-specific password, not your Apple Account password. Create one for zmail in your Apple Account settings.",
            "https://support.apple.com/en-us/102654",
        )),
        Provider::Fastmail => Some((
            "Fastmail needs an app password, not your Fastmail password. Create one with mail access in Fastmail's settings.",
            "https://www.fastmail.help/hc/en-us/articles/360058752854",
        )),
        Provider::Microsoft365 => None,
    }
}

impl Render for AccountSetupView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let body = match self.stage {
            Stage::Welcome => self.render_welcome(&colors, cx),
            Stage::Provider => self.render_provider(&colors, cx),
            Stage::Credentials => self.render_credentials(&colors, cx),
            Stage::Discovering => self.render_discovering(&colors),
            Stage::Consent => self.render_consent(&colors, cx),
            Stage::Result => self.render_result(&colors, cx),
            Stage::Manual => self.render_manual(&colors, cx),
            Stage::Colour => self.render_colour(&colors, cx),
            Stage::Syncing => self.render_syncing(&colors),
        };
        let footer = self.render_footer(cx);

        div()
            .id("account-setup")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::continue_action))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .text_color(rgb(colors.foreground))
            .child(
                div()
                    .id("account-setup-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .items_center()
                    .px(px(SPACE_6))
                    .py(px(SPACE_8))
                    .child(
                        column()
                            .gap(px(SPACE_6))
                            .when_some(self.notice.clone(), |this, notice| {
                                this.child(
                                    Banner::new("account-setup-notice", notice)
                                        .tone(BannerTone::Info),
                                )
                            })
                            .child(body),
                    ),
            )
            .when_some(footer, |this, footer| {
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .justify_center()
                        .px(px(SPACE_6))
                        .py(px(SPACE_3))
                        .border_t_1()
                        .border_color(rgb(colors.border))
                        .bg(rgb(colors.surface))
                        .child(footer),
                )
            })
    }
}

/// The flow's centred column.
fn column() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .w_full()
        .max_w(rems_from_px(COLUMN_WIDTH))
        .min_w_0()
}

/// A stage's title and the sentence under it.
fn heading(
    title: impl Into<SharedString>,
    detail: impl Into<SharedString>,
    colors: &UiColors,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_2))
        .child({
            let title = title.into();
            div()
                .id("account-setup-heading")
                .accessible(Role::Heading, title.clone())
                .aria_level(1)
                .type_style(TYPE_HEADLINE)
                .font_weight(FontWeight::SEMIBOLD)
                .child(title)
        })
        .child(
            div()
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .child(readable("account-setup-detail", detail)),
        )
}

/// A labelled text field with its error underneath.
fn field(view: &AccountSetupView, field: AccountField, colors: &UiColors) -> gpui::Div {
    input_card(field.label(), view.form.input(field), colors)
        .children(error_line(view, field, colors))
}

/// The error for `field`, if the last check found one.
fn error_line(
    view: &AccountSetupView,
    field: AccountField,
    colors: &UiColors,
) -> Option<gpui::Stateful<gpui::Div>> {
    view.form.errors.get(&field).map(|error| {
        let text = SharedString::from(format!("{}: {error}", field.label()));
        div()
            .id(SharedString::from(format!("account-setup-error-{field:?}")))
            .debug_selector(move || format!("account-setup-error-{field:?}"))
            // Read out as soon as it appears, naming the field.
            .accessible(Role::Alert, text.clone())
            .aria_value(text)
            .a11y_flags(A11yFlags {
                live: Some(Politeness::Assertive),
                ..A11yFlags::default()
            })
            .type_style(TYPE_CAPTION)
            .text_color(rgb(colors.danger))
            .child(error.clone())
    })
}

/// A labelled control (a Select) with its error underneath.
fn labelled(label: &str, control: impl IntoElement, colors: &UiColors) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w_full()
        .min_w_0()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(colors.muted))
                .child(label.to_owned()),
        )
        .child(control)
}

/// Two controls side by side; the second is narrow (a port, a Select).
fn pair(wide: impl IntoElement, narrow: impl IntoElement, narrow_width: f32) -> gpui::Div {
    div()
        .flex()
        .gap(px(SPACE_3))
        .w_full()
        .child(div().flex_1().min_w_0().child(wide))
        .child(
            div()
                .flex_none()
                .w(rems_from_px(narrow_width))
                .child(narrow),
        )
}

/// A muted caption.
fn caption(text: impl Into<SharedString>, colors: &UiColors) -> gpui::Div {
    let text = text.into();
    div()
        .type_style(TYPE_CAPTION)
        .text_color(rgb(colors.muted))
        .child(readable(
            SharedString::from(format!("account-setup-caption-{text}")),
            text,
        ))
}

/// A spinning loader icon.
fn spinner(id: &'static str, color: u32) -> AnyElement {
    icon("loader", ICON_SIZE_SM, color)
        .with_animation(id, Animation::new(SPIN_PERIOD).repeat(), |glyph, delta| {
            glyph.with_transformation(Transformation::rotate(percentage(delta)))
        })
        .into_any_element()
}

/// How a server is described: `imap.example.com:993 · TLS`.
fn describe_server(server: &ServerSettings) -> String {
    format!(
        "{}:{} · {}",
        server.host,
        server.port,
        server.security.label()
    )
}

/// Where settings came from, as a sentence ending.
fn describe_source(source: &Source) -> String {
    match source {
        Source::Provider(provider) => format!("zmail's built-in {provider} settings"),
        other => other.to_string(),
    }
}

impl AccountSetupView {
    fn render_welcome(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_4))
            .pt(px(SPACE_8))
            .text_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(EMPTY_STATE_ICON_FRAME_SIZE))
                    .rounded_full()
                    .bg(rgb(colors.button_background))
                    .child(icon("mail", ICON_SIZE_LG * 1.5, colors.accent)),
            )
            .child(heading(
                "Welcome to Zmail",
                "A fast mail client that keeps your mail on this computer. Remote images and trackers stay blocked until you allow them.",
                colors,
            ).items_center())
            .child(
                div().debug_selector(|| "account-setup-get-started".into()).child(
                    Button::new("account-setup-get-started", "Add your first account")
                        .primary()
                        .size(ButtonSize::Large)
                        .icon("user-plus")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.advance(window, cx);
                        })),
                ),
            )
            .into_any_element()
    }

    fn render_provider(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let tile = |id: &'static str,
                    label: &'static str,
                    hint: &'static str,
                    provider: Option<Provider>,
                    cx: &mut Context<Self>| {
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_1))
                .debug_selector(move || id.into())
                .child(
                    Button::new(id, label)
                        .size(ButtonSize::Large)
                        .full_width()
                        .icon(if provider.is_some() { "mail" } else { "server" })
                        .selected(
                            self.form.provider == provider
                                && self.history.contains(&Stage::Credentials),
                        )
                        .tooltip(hint)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.choose_provider(provider, window, cx);
                        })),
                )
                .child(caption(hint, colors).pl(px(SPACE_2)))
        };
        let mut tiles = Vec::new();
        for (provider, id, hint) in [
            (
                Provider::Google,
                "account-setup-provider-google",
                "Gmail and Google Workspace",
            ),
            (
                Provider::Microsoft365,
                "account-setup-provider-microsoft365",
                "Outlook.com and work or school accounts",
            ),
            (
                Provider::Fastmail,
                "account-setup-provider-fastmail",
                "Fastmail, over JMAP",
            ),
            (
                Provider::ICloud,
                "account-setup-provider-icloud",
                "iCloud Mail",
            ),
        ] {
            tiles.push(tile(id, provider.name(), hint, Some(provider), cx));
        }
        tiles.push(tile(
            "account-setup-provider-other",
            "Other",
            "Any IMAP or JMAP provider; zmail looks up its settings",
            None,
            cx,
        ));
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_6))
            .child(heading(
                "Add a mail account",
                "Who provides your email?",
                colors,
            ))
            .child(div().flex().flex_col().gap(px(SPACE_3)).children(tiles))
            .into_any_element()
    }

    fn render_credentials(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let provider = self.form.provider;
        let title = match provider {
            Some(provider) => format!("Sign in to {provider}"),
            None => "Sign in".to_owned(),
        };
        let note = provider.and_then(app_password_help).map(|(message, url)| {
            Banner::new("account-setup-app-password", message)
                .tone(BannerTone::Info)
                .title("Use an app password")
                .action(
                    Button::new("account-setup-app-password-help", "How to create one")
                        .icon("external-link")
                        .on_click(move |_, _, cx: &mut App| cx.open_url(url)),
                )
        });
        let oauth_note = (provider == Some(Provider::Microsoft365)).then(|| {
            Banner::new(
                "account-setup-oauth-only",
                "Microsoft 365 only allows OAuth sign-in. zmail doesn't have its own Microsoft app registration yet, so enter an OAuth client ID from your organisation under Advanced.",
            )
            .tone(BannerTone::Caution)
            .title("OAuth sign-in needed")
        });
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                title,
                "Your password stays on this computer, in the system keychain.",
                colors,
            ))
            .children(note)
            .children(oauth_note)
            .child(field(self, AccountField::Name, colors))
            .child(field(self, AccountField::Email, colors))
            .when(!self.form.use_oauth, |this| {
                this.child(field(self, AccountField::Password, colors))
            })
            .child(self.render_advanced(colors, cx))
            .into_any_element()
    }

    /// The sign-in step's Advanced section: password or OAuth2, and the
    /// OAuth2 client fields.
    fn render_advanced(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let open = self.advanced_open;
        let preset = matches!(
            self.form.provider,
            Some(Provider::Google | Provider::Microsoft365)
        );
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_3))
            .child(
                // A row, so the button keeps its own width.
                div().flex().debug_selector(|| "account-setup-advanced".into()).child(
                    Button::new("account-setup-advanced", "Advanced")
                        .ghost()
                        .icon(if open { "chevron-down" } else { "chevron-right" })
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.toggle_advanced(cx);
                        })),
                ),
            )
            .when(open, |this| {
                this.child(
                    section_card("Sign-in method", colors)
                        .child(
                            div()
                                .flex()
                                .gap(px(SPACE_2))
                                .child(
                                    Button::new("account-setup-auth-password", "Password")
                                        .icon("key")
                                        .selected(!self.form.use_oauth)
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.set_use_oauth(false, cx);
                                        })),
                                )
                                .child(
                                    div().debug_selector(|| "account-setup-auth-oauth".into()).child(
                                        Button::new("account-setup-auth-oauth", "OAuth")
                                            .icon("shield")
                                            .selected(self.form.use_oauth)
                                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                                this.set_use_oauth(true, cx);
                                            })),
                                    ),
                                ),
                        )
                        .when(self.form.use_oauth, |this| {
                            this.child(caption(
                                "zmail has no OAuth client of its own yet, so use one registered with your provider. Continuing opens your provider's sign-in page in the browser.",
                                colors,
                            ))
                            .child(field(self, AccountField::OauthClientId, colors))
                            .child(field(self, AccountField::OauthClientSecret, colors))
                            .when(!preset, |this| {
                                this.child(field(self, AccountField::OauthAuthorizationEndpoint, colors))
                                    .child(field(self, AccountField::OauthTokenEndpoint, colors))
                                    .child(field(self, AccountField::OauthScopes, colors))
                            })
                        }),
                )
            })
            .into_any_element()
    }

    fn render_discovering(&self, colors: &UiColors) -> AnyElement {
        let Some(discovery) = &self.discovery else {
            return div().into_any_element();
        };
        let domain = discovery.email().domain().to_owned();
        let rows = discovery.steps().into_iter().map(|step| {
            let report = discovery
                .reports()
                .iter()
                .find(|report| report.step == step);
            let running = self.running_step == Some(step);
            let (glyph, detail): (AnyElement, Option<String>) =
                match report.map(|report| &report.outcome) {
                    _ if running => (spinner("account-setup-step-spinner", colors.info), None),
                    Some(StepOutcome::Found { note }) => (
                        icon("check-circle", ICON_SIZE_SM, colors.success).into_any_element(),
                        note.clone(),
                    ),
                    Some(StepOutcome::NothingFound { note }) => (
                        icon("minus", ICON_SIZE_SM, colors.muted).into_any_element(),
                        Some(note.clone().unwrap_or_else(|| "Nothing found".to_owned())),
                    ),
                    Some(StepOutcome::Failed(error)) => (
                        icon("alert-circle", ICON_SIZE_SM, colors.warning).into_any_element(),
                        Some(short_error(error)),
                    ),
                    Some(StepOutcome::Skipped) => (
                        icon("minus", ICON_SIZE_SM, colors.muted).into_any_element(),
                        Some("Skipped: you chose not to ask Mozilla".to_owned()),
                    ),
                    None => (
                        icon("clock", ICON_SIZE_SM, colors.muted).into_any_element(),
                        None,
                    ),
                };
            div()
                .debug_selector(move || format!("account-setup-step-{step:?}"))
                .flex()
                .gap(px(SPACE_3))
                .py(px(SPACE_2))
                .child(div().pt(px(2.0)).child(glyph))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(SPACE_1))
                        .child(
                            div()
                                .type_style(if running { TYPE_BODY_STRONG } else { TYPE_BODY })
                                .text_color(rgb(if report.is_none() && !running {
                                    colors.muted
                                } else {
                                    colors.foreground
                                }))
                                .child(step_label(step, &domain)),
                        )
                        .children(detail.map(|detail| caption(detail, colors))),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Finding your settings",
                format!(
                    "zmail is asking {domain} how to connect. Your password isn't sent anywhere until zmail signs in."
                ),
                colors,
            ))
            .child(
                div()
                    .id("account-setup-steps")
                    .role(Role::List)
                    .flex()
                    .flex_col()
                    .px(px(SPACE_4))
                    .py(px(SPACE_2))
                    .rounded_lg()
                    .bg(rgb(colors.surface))
                    .border_1()
                    .border_color(rgb(colors.border))
                    .children(rows),
            )
            .into_any_element()
    }

    fn render_consent(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let domain = self
            .discovery
            .as_ref()
            .map(|discovery| discovery.email().domain().to_owned())
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Ask Mozilla's database?",
                format!("{domain} doesn't publish its mail settings."),
                colors,
            ))
            .child(
                section_card("Mozilla's provider database", colors)
                    .child(div().type_style(TYPE_BODY).child(
                        "Thunderbird's provider database (ISPDB) has the settings of many mail providers.",
                    ))
                    .child(div().type_style(TYPE_BODY).child(format!(
                        "To look them up, zmail sends your mail domain, {domain}, to Mozilla at autoconfig.thunderbird.net. It doesn't send your address or your password."
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(SPACE_2))
                            .child(
                                div().debug_selector(|| "account-setup-consent-yes".into()).child(
                                    Button::new("account-setup-consent-yes", "Ask Mozilla")
                                        .primary()
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.answer_consent(true, window, cx);
                                        })),
                                ),
                            )
                            .child(
                                div().debug_selector(|| "account-setup-consent-no".into()).child(
                                    Button::new("account-setup-consent-no", "Don't ask Mozilla")
                                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.answer_consent(false, window, cx);
                                        })),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_result(&self, colors: &UiColors, _cx: &mut Context<Self>) -> AnyElement {
        let Some(found) = &self.found else {
            return div().into_any_element();
        };
        let server_card = |title: &str, server: &ServerSettings, source: &Source| {
            section_card(title, colors)
                .child(div().type_style(TYPE_BODY).child(describe_server(server)))
                .child(caption(format!("User name: {}", server.username), colors))
                .child(caption(
                    format!("Found in {}", describe_source(source)),
                    colors,
                ))
        };
        let (cards, guessed, oauth_only) = match found {
            MailSettings::Jmap(jmap) => (
                vec![
                    section_card("JMAP", colors)
                        .child(
                            div()
                                .type_style(TYPE_BODY)
                                .child(jmap.value.session_url.clone()),
                        )
                        .child(caption(
                            format!("Found in {}", describe_source(&jmap.source)),
                            colors,
                        )),
                ],
                false,
                false,
            ),
            MailSettings::ImapSmtp { imap, smtp } => (
                vec![
                    server_card("Incoming mail (IMAP)", &imap.value, &imap.source),
                    server_card("Outgoing mail (SMTP)", &smtp.value, &smtp.source),
                ],
                [&imap.source, &smtp.source]
                    .into_iter()
                    .any(|source| *source == Source::HostnameGuess),
                imap.value.auth == AuthMethod::OAuth2 && !self.form.use_oauth,
            ),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Settings found",
                "zmail will connect with these settings. Check them, or edit them if your provider says otherwise.",
                colors,
            ))
            .when(guessed, |this| {
                this.child(
                    Banner::new(
                        "account-setup-guessed",
                        "These servers answered on the usual ports, but nothing confirmed they are your provider's. If signing in fails, check them against your provider's help pages.",
                    )
                    .tone(BannerTone::Caution)
                    .title("Best guess"),
                )
            })
            .when(oauth_only, |this| {
                this.child(
                    Banner::new(
                        "account-setup-oauth-hint",
                        "Your provider lists only OAuth sign-in. If your password is refused, create an app password, or go back and set up OAuth under Advanced.",
                    )
                    .tone(BannerTone::Info),
                )
            })
            .children(cards)
            .into_any_element()
    }

    fn render_manual(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let discovery_failed = self
            .discovery
            .as_ref()
            .is_some_and(|discovery| discovery.result().is_none());
        let domain = self
            .discovery
            .as_ref()
            .map(|discovery| discovery.email().domain().to_owned());
        let cleartext = self.form.values(cx).uses_cleartext();
        let servers = match self.form.protocol {
            ProtocolKind::Jmap => vec![
                section_card("JMAP", colors).child(field(self, AccountField::JmapUrl, colors)),
            ],
            ProtocolKind::Imap => vec![
                section_card("Incoming mail (IMAP)", colors)
                    .child(pair(
                        field(self, AccountField::ImapHost, colors),
                        field(self, AccountField::ImapPort, colors),
                        96.0,
                    ))
                    .child(pair(
                        field(self, AccountField::ImapUsername, colors),
                        labelled("Security", self.imap_security_select.clone(), colors)
                            .children(error_line(self, AccountField::ImapSecurity, colors)),
                        180.0,
                    ))
                    .child(caption(
                        "zmail connects to IMAP over TLS, usually on port 993. STARTTLS on port 143 isn't supported yet.",
                        colors,
                    )),
                section_card("Outgoing mail (SMTP)", colors)
                    .child(pair(
                        field(self, AccountField::SmtpHost, colors),
                        field(self, AccountField::SmtpPort, colors),
                        96.0,
                    ))
                    .child(pair(
                        field(self, AccountField::SmtpUsername, colors),
                        labelled("Security", self.smtp_security_select.clone(), colors)
                            .children(error_line(self, AccountField::SmtpSecurity, colors)),
                        180.0,
                    )),
            ],
        };
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Server settings",
                "Enter the settings from your provider's help pages.",
                colors,
            ))
            .when_some(domain.filter(|_| discovery_failed), |this, domain| {
                this.child(
                    Banner::new(
                        "account-setup-not-found",
                        format!("zmail couldn't find the settings for {domain}. The servers below are the usual names; check them before you continue."),
                    )
                    .tone(BannerTone::Caution)
                    .title("Settings not found"),
                )
            })
            // Above the servers, where it can't be scrolled past.
            .when(cleartext, |this| this.child(self.render_cleartext_warning(colors, cx)))
            .child(labelled("Protocol", self.protocol_select.clone(), colors).max_w(rems_from_px(240.0)))
            .children(servers)
            .into_any_element()
    }

    /// The warning and opt-in an unencrypted connection needs.
    fn render_cleartext_warning(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_2))
            .child(
                Banner::new(
                    "account-setup-cleartext",
                    "Without encryption your password and your mail cross the network as plain text, and anyone on the way can read or change them. Only use this for a server on your own computer or network.",
                )
                .tone(BannerTone::Danger)
                .title("Unencrypted connection")
                .action(
                    div().debug_selector(|| "account-setup-allow-cleartext".into()).child(
                        Button::new("account-setup-allow-cleartext", "Connect without encryption")
                            .icon(if self.form.allow_cleartext { "check" } else { "square" })
                            .selected(self.form.allow_cleartext)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.toggle_cleartext(cx);
                            })),
                    ),
                ),
            )
            .children(error_line(self, AccountField::AllowCleartext, colors))
            .into_any_element()
    }

    fn render_colour(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let email = self.form.text(AccountField::Email, cx);
        let swatches = AccountColor::ALL.into_iter().map(|color| {
            let selected = self.color == color;
            div()
                .id(SharedString::from(format!(
                    "account-setup-colour-{}",
                    color.as_str()
                )))
                .hit_target(move || format!("account-setup-colour-{}", color.as_str()))
                .accessible(Role::RadioButton, color.label())
                .aria_toggled(if selected {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .tab_index(0)
                .flex()
                .items_center()
                .justify_center()
                .size(px(SWATCH_SIZE + 8.0))
                .rounded_full()
                .border_2()
                .border_color(rgb(if selected {
                    colors.foreground
                } else {
                    colors.background
                }))
                .cursor_pointer()
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.pick_colour(color, cx)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(SWATCH_SIZE))
                        .rounded_full()
                        .bg(rgb(account_color(color)))
                        .when(selected, |this| {
                            this.child(icon("check", ICON_SIZE_SM, 0xFFFFFF))
                        }),
                )
        });
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_6))
            .child(heading(
                "Pick a colour",
                format!("The colour marks {email} in the sidebar and in the Unified Inbox."),
                colors,
            ))
            .child(
                div()
                    .id("account-setup-colours")
                    .accessible(Role::RadioGroup, "Account colour")
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_3))
                    .children(swatches),
            )
            .child(caption(self.color.label(), colors))
            .into_any_element()
    }

    fn render_syncing(&self, colors: &UiColors) -> AnyElement {
        let sync = &self.sync;
        let (glyph, status) = if let Some(error) = &sync.error {
            (
                icon("alert-circle", ICON_SIZE_SM, colors.danger).into_any_element(),
                format!("The first sync failed: {error}"),
            )
        } else if sync.done {
            (
                icon("check-circle", ICON_SIZE_SM, colors.success).into_any_element(),
                "Your mail is here.".to_owned(),
            )
        } else {
            let phase = sync.phase.map_or("Starting", SyncPhase::label);
            let status = if sync.detail.is_empty() {
                format!("{phase}…")
            } else {
                format!("{phase}: {}", sync.detail)
            };
            (spinner("account-setup-sync-spinner", colors.info), status)
        };
        let counts = format!(
            "{} {} · {} {}",
            sync.folders,
            if sync.folders == 1 {
                "folder"
            } else {
                "folders"
            },
            sync.conversations,
            if sync.conversations == 1 {
                "conversation"
            } else {
                "conversations"
            },
        );
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_4))
            .child(heading(
                "Getting your mail",
                "zmail copies your recent mail to this computer. You can start reading while it works; the rest arrives in the background.",
                colors,
            ))
            .child(
                section_card("First sync", colors)
                    .debug_selector(|| "account-setup-sync".into())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_2))
                            .child(glyph)
                            .child(div().flex_1().min_w_0().type_style(TYPE_BODY).child(status)),
                    )
                    .child(caption(counts, colors)),
            )
            .when(sync.error.is_some(), |this| {
                this.child(caption(
                    "The account is saved. Fix it later with \"Fix…\" beside it in the sidebar.",
                    colors,
                ))
            })
            .into_any_element()
    }

    /// The footer: Back on the left, the stage's actions on the right.
    /// `None` when the stage has neither (its actions are in the body).
    fn render_footer(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let primary = |id: &'static str, label: &'static str, cx: &mut Context<Self>| {
            div().debug_selector(move || id.into()).child(
                Button::new(id, label)
                    .primary()
                    .shortcut("secondary-enter")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.advance(window, cx);
                    })),
            )
        };
        let manual = |cx: &mut Context<Self>| {
            div()
                .debug_selector(|| "account-setup-manual".into())
                .child(
                    Button::new("account-setup-manual", "Enter settings manually")
                        .ghost()
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.discovery_task = None;
                            this.running_step = None;
                            if let Some(email) = this.discovery.as_ref().map(|d| d.email().clone())
                            {
                                this.form.prefill_guesses(&email, cx);
                            }
                            this.edit_manually(window, cx);
                        })),
                )
        };
        let actions: Vec<AnyElement> = match self.stage {
            Stage::Welcome | Stage::Provider => Vec::new(),
            Stage::Credentials => {
                vec![primary("account-setup-continue", "Continue", cx).into_any_element()]
            }
            Stage::Discovering | Stage::Consent => vec![manual(cx).into_any_element()],
            Stage::Result => vec![
                div()
                    .debug_selector(|| "account-setup-edit".into())
                    .child(Button::new("account-setup-edit", "Edit settings").on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.edit_manually(window, cx);
                        }),
                    ))
                    .into_any_element(),
                primary("account-setup-add", "Add account", cx).into_any_element(),
            ],
            Stage::Manual => {
                vec![primary("account-setup-add", "Add account", cx).into_any_element()]
            }
            Stage::Colour => {
                vec![primary("account-setup-continue", "Continue", cx).into_any_element()]
            }
            Stage::Syncing => vec![
                primary(
                    "account-setup-done",
                    if self.first_run {
                        "Start using Zmail"
                    } else {
                        "Done"
                    },
                    cx,
                )
                .into_any_element(),
            ],
        };
        if actions.is_empty() && !self.can_go_back() {
            return None;
        }
        let footer = column()
            .flex_row()
            .items_center()
            .gap(px(SPACE_2))
            .when(self.can_go_back(), |this| {
                this.child(
                    div().debug_selector(|| "account-setup-back".into()).child(
                        Button::new("account-setup-back", "Back")
                            .ghost()
                            .icon("arrow-left")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.back(window, cx);
                            })),
                    ),
                )
            })
            .child(div().flex_1())
            .children(actions);
        Some(footer)
    }
}

/// The progress list's line for a step.
fn step_label(step: Step, domain: &str) -> String {
    match step {
        Step::DnsSrv => format!("Checking DNS records for {domain}"),
        Step::DomainAutoconfig => format!("Asking {domain} for its settings"),
        Step::WellKnownJmap => format!("Checking {domain} for JMAP"),
        Step::Ispdb => "Checking Mozilla's provider database".to_owned(),
        Step::MxGuess => format!("Checking where mail for {domain} goes"),
        Step::HostnameGuess => "Trying the usual server names".to_owned(),
    }
}

/// The last, most specific part of a network error, which is the part a
/// person can act on ("connection refused", "certificate not valid").
fn short_error(error: &str) -> String {
    let first = error.split("; ").next().unwrap_or(error);
    let last = first.rsplit(": ").next().unwrap_or(first).trim();
    let mut short = last.to_owned();
    if let Some(first) = short.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    short
}

#[cfg(test)]
mod tests {
    use super::short_error;

    #[test]
    fn network_errors_are_shortened_to_their_cause() {
        assert_eq!(
            short_error(
                "error sending request for url (https://autoconfig.example.com/mail/config-v1.1.xml): client error (Connect): dns error: no record found; other"
            ),
            "No record found"
        );
        assert_eq!(short_error("timed out"), "Timed out");
    }
}
