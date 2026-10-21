//! `RecipientField`: a To, Cc or Bcc field that shows each address as a
//! chip, with autocomplete from the local address history.
//!
//! - Typing a comma or semicolon, pressing Enter, or leaving the field
//!   turns the typed text into chips. Pasting a list (commas, semicolons
//!   or one address per line) makes a chip of every address at once.
//! - While the field has text, up to [`MAX_SUGGESTIONS`] addresses from
//!   [`LocalStore::address_suggestions`] show below it, best first. Up and
//!   Down move the highlight, Enter or a click adds the highlighted one,
//!   and Escape closes the list. Suggestions are read from the local store
//!   only.
//! - Backspace in the empty field removes the last chip, and each chip has
//!   a remove button.
//! - A chip whose text is not a valid address is drawn in the danger
//!   colour, so it can be fixed before sending.
//!
//! The field emits [`RecipientFieldEvent::Changed`] whenever its recipients
//! change, and [`RecipientField::recipients`] returns them, typed text
//! included, as address list entries ready to save.

use std::sync::Arc;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyContext,
    MouseButton, Render, Role, SharedString, Subscription, Window, div, prelude::*, px, rgb,
};
use zmail_core::{AddressSuggestion, participants::Participant, participants::parse_address_list};
use zmail_store::LocalStore;

use crate::a11y::{
    AccessibleExt, Politeness, announcement_key, highlight_announcement, live_region,
};
use crate::components::actions::{Cancel, SelectNext, SelectPrevious};
use crate::components::menu::MENU_ITEM_HEIGHT;
use crate::components::popover::{Popover, popover_below};
use crate::components::text_input::{Backspace, Paste};
use crate::components::{ComponentTheme, TextInput, TextInputEvent};
use crate::styles::{
    ICON_SIZE_SM, RADIUS_FULL, RADIUS_MD, RADIUS_SM, SPACE_0_5, SPACE_1, SPACE_2, TYPE_BODY,
    TYPE_CAPTION, TextScale, TypeStyleExt, rems_from_px,
};
use crate::widgets::{EllipsisExt, icon};

/// The key context a recipient field sets.
pub(crate) const KEY_CONTEXT: &str = "RecipientField";
/// Added to [`KEY_CONTEXT`] while suggestions show, so Up, Down and Escape
/// work the list instead of the surrounding view.
pub(crate) const SUGGESTIONS_CONTEXT: &str = "showing_suggestions";
/// The most suggestions shown at once.
pub(crate) const MAX_SUGGESTIONS: usize = 6;
/// Narrowest the text part of the field gets beside the chips.
const INPUT_MIN_WIDTH: f32 = 120.0;
/// Widest a suggestion list gets.
const SUGGESTIONS_WIDTH: f32 = 360.0;

/// One recipient chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recipient {
    participant: Participant,
}

impl Recipient {
    /// Every recipient in `text`, an address list.
    pub(crate) fn parse_list(text: &str) -> Vec<Self> {
        parse_address_list(text)
            .into_iter()
            .filter(|participant| participant.email.is_some() || participant.name.is_some())
            .map(|participant| Self { participant })
            .collect()
    }

    /// The recipient as an address list entry, for saving.
    pub(crate) fn header_value(&self) -> String {
        self.participant.to_header_value()
    }

    /// The chip text: the display name, else the address.
    fn label(&self) -> &str {
        self.participant.display_name()
    }

    /// The lowercased address, for spotting duplicates.
    fn key(&self) -> String {
        self.participant.key()
    }

    /// Whether the address looks deliverable: one `@` with text on both
    /// sides and no spaces, and a domain that neither starts nor ends with
    /// a dot nor has two in a row.
    pub(crate) fn is_valid(&self) -> bool {
        let Some(email) = self.participant.email.as_deref() else {
            return false;
        };
        let Some((local, domain)) = email.split_once('@') else {
            return false;
        };
        !local.is_empty()
            && !domain.is_empty()
            && !domain.contains('@')
            && !email.contains(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | ','))
            && !domain.starts_with('.')
            && !domain.ends_with('.')
            && !domain.contains("..")
    }
}

/// What a [`RecipientField`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecipientFieldEvent {
    /// The recipients or the typed text changed.
    Changed,
}

/// A recipient field: chips, a text input and its suggestions.
pub(crate) struct RecipientField {
    store: Arc<LocalStore>,
    label: SharedString,
    chips: Vec<Recipient>,
    input: Entity<TextInput>,
    suggestions: Vec<AddressSuggestion>,
    /// Index into `suggestions`.
    highlighted: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RecipientFieldEvent> for RecipientField {}

impl Focusable for RecipientField {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl RecipientField {
    /// A field labelled `label` ("To", "Cc", "Bcc") holding `recipients`
    /// (address list entries, as a draft stores them).
    pub(crate) fn new(
        store: Arc<LocalStore>,
        label: &'static str,
        recipients: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(cx)
                .borderless()
                .compact()
                .label(label)
                .placeholder("")
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            // Leaving the field keeps what was typed, as chips.
            cx.on_blur(&input.focus_handle(cx), window, |this, _, cx| {
                this.commit_text(cx);
                this.close_suggestions(cx);
            }),
        ];
        Self {
            store,
            label: label.into(),
            chips: recipients
                .iter()
                .flat_map(|recipient| Recipient::parse_list(recipient))
                .collect(),
            input,
            suggestions: Vec::new(),
            highlighted: 0,
            _subscriptions: subscriptions,
        }
    }

    /// The recipients to save: every chip, then whatever is still typed.
    pub(crate) fn recipients(&self, cx: &App) -> Vec<String> {
        self.chips
            .iter()
            .chain(&Recipient::parse_list(self.input.read(cx).text()))
            .map(Recipient::header_value)
            .collect()
    }

    /// The chips.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn chips(&self) -> &[Recipient] {
        &self.chips
    }

    /// The text input.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn input(&self) -> &Entity<TextInput> {
        &self.input
    }

    /// The suggestions showing, best first.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn suggestions(&self) -> &[AddressSuggestion] {
        &self.suggestions
    }

    /// Moves keyboard focus to the field's text input.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_input_event(
        &mut self,
        _: &Entity<TextInput>,
        event: &TextInputEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Changed => {
                let text = self.input.read(cx).text().to_owned();
                if let Some(split) = last_separator(&text) {
                    // A comma or semicolon ends the addresses before it.
                    self.add_chips(&text[..split], cx);
                    let rest = text[split + 1..].trim_start().to_owned();
                    self.input.update(cx, |input, cx| input.set_text(rest, cx));
                } else if !text.is_empty() && text.trim().is_empty() {
                    // The space typed after a comma.
                    self.input.update(cx, |input, cx| input.set_text("", cx));
                }
                self.refresh_suggestions(cx);
                cx.emit(RecipientFieldEvent::Changed);
            }
            TextInputEvent::Submitted => {
                if self.suggestions.is_empty() {
                    self.commit_text(cx);
                } else {
                    self.accept_suggestion(self.highlighted, cx);
                }
            }
        }
    }

    /// Turns the typed text into chips.
    fn commit_text(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).text().to_owned();
        if text.trim().is_empty() {
            return;
        }
        self.add_chips(&text, cx);
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.close_suggestions(cx);
        cx.emit(RecipientFieldEvent::Changed);
    }

    /// Adds a chip for each address in `text`, skipping ones already here.
    fn add_chips(&mut self, text: &str, cx: &mut Context<Self>) {
        for recipient in Recipient::parse_list(text) {
            let key = recipient.key();
            if !self.chips.iter().any(|chip| chip.key() == key) {
                self.chips.push(recipient);
            }
        }
        cx.notify();
    }

    /// Removes the chip at `index`.
    pub(crate) fn remove_chip(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.chips.len() {
            self.chips.remove(index);
            cx.emit(RecipientFieldEvent::Changed);
            cx.notify();
        }
    }

    fn refresh_suggestions(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).text().trim().to_owned();
        self.suggestions = if query.is_empty() {
            Vec::new()
        } else {
            // A few spare, in case some are already chips.
            self.store
                .address_suggestions(&query, MAX_SUGGESTIONS + self.chips.len())
                .unwrap_or_default()
                .into_iter()
                .filter(|suggestion| !self.chips.iter().any(|chip| chip.key() == suggestion.email))
                .take(MAX_SUGGESTIONS)
                .collect()
        };
        self.highlighted = 0;
        cx.notify();
    }

    fn close_suggestions(&mut self, cx: &mut Context<Self>) {
        if !self.suggestions.is_empty() {
            self.suggestions.clear();
            cx.notify();
        }
    }

    /// Adds the suggestion at `index` as a chip, replacing the typed text.
    fn accept_suggestion(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(suggestion) = self.suggestions.get(index) else {
            return;
        };
        let value = suggestion.to_header_value();
        self.add_chips(&value, cx);
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.close_suggestions(cx);
        cx.emit(RecipientFieldEvent::Changed);
    }

    /// Backspace in the empty field removes the last chip; otherwise the
    /// text input handles it.
    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        if input.text().is_empty() && !self.chips.is_empty() {
            self.remove_chip(self.chips.len() - 1, cx);
            cx.stop_propagation();
        }
    }

    /// Pasting a list makes a chip of every address in it; pasting one
    /// address (or other text) is left to the text input.
    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let list = text.replace(['\n', '\r'], ",");
        if last_separator(&list).is_none() {
            return;
        }
        cx.stop_propagation();
        let typed = self.input.read(cx).text().to_owned();
        self.add_chips(&format!("{typed},{list}"), cx);
        self.input.update(cx, |input, cx| input.set_text("", cx));
        self.close_suggestions(cx);
        cx.emit(RecipientFieldEvent::Changed);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if !self.suggestions.is_empty() {
            self.highlighted = (self.highlighted + 1) % self.suggestions.len();
            cx.notify();
        }
    }

    fn select_previous(&mut self, _: &SelectPrevious, _: &mut Window, cx: &mut Context<Self>) {
        if !self.suggestions.is_empty() {
            self.highlighted =
                (self.highlighted + self.suggestions.len() - 1) % self.suggestions.len();
            cx.notify();
        }
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        self.close_suggestions(cx);
    }
}

/// The byte offset of the last comma or semicolon in `text` outside
/// quotes and angle brackets, so `"Lee, Carol" <carol@example.com>` stays
/// one address.
fn last_separator(text: &str) -> Option<usize> {
    let mut in_quotes = false;
    let mut in_angle = false;
    let mut escaped = false;
    let mut last = None;
    for (offset, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '<' if !in_quotes => in_angle = true,
            '>' if !in_quotes => in_angle = false,
            ',' | ';' if !in_quotes && !in_angle => last = Some(offset),
            _ => {}
        }
    }
    last
}

impl Render for RecipientField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = ComponentTheme::get(cx).colors;
        let focused = self.input.focus_handle(cx).is_focused(window);
        let showing = !self.suggestions.is_empty() && focused;
        let field_id = self.label.clone();

        let chips = self.chips.iter().enumerate().map(|(index, chip)| {
            let valid = chip.is_valid();
            let (background, border, foreground) = if valid {
                (colors.button_background, colors.border, colors.foreground)
            } else {
                (colors.danger_background, colors.danger, colors.danger)
            };
            let full = SharedString::from(chip.header_value());
            div()
                .id(SharedString::from(format!("recipient-{field_id}-{index}")))
                .debug_selector({
                    let field_id = field_id.clone();
                    move || format!("recipient-{field_id}-{index}")
                })
                .flex()
                .flex_none()
                .items_center()
                .gap(px(SPACE_1))
                .max_w(rems_from_px(SUGGESTIONS_WIDTH))
                .pl(px(SPACE_2))
                .pr(px(SPACE_0_5))
                .py(px(SPACE_0_5))
                .rounded(px(RADIUS_FULL))
                .bg(rgb(background))
                .border_1()
                .border_color(rgb(border))
                .text_color(rgb(foreground))
                .type_style(TYPE_BODY)
                .accessible(
                    Role::ListItem,
                    if valid {
                        full.clone()
                    } else {
                        SharedString::from(format!("{full} (not a valid address)"))
                    },
                )
                .tooltip(crate::components::tooltip::Tooltip::text(if valid {
                    full.clone()
                } else {
                    SharedString::from(format!("{full} is not a valid address"))
                }))
                .when(!valid, |this| {
                    this.child(icon("alert-circle", ICON_SIZE_SM, colors.danger))
                })
                .child(div().min_w_0().ellipsis().child(chip.label().to_owned()))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "recipient-{field_id}-{index}-remove"
                        )))
                        .debug_selector({
                            let field_id = field_id.clone();
                            move || format!("recipient-{field_id}-{index}-remove")
                        })
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(ICON_SIZE_SM + SPACE_1))
                        .rounded(px(RADIUS_FULL))
                        .cursor_pointer()
                        .hover(|this| this.bg(rgb(colors.hover_background)))
                        .accessible(
                            Role::Button,
                            SharedString::from(format!("Remove {}", chip.label())),
                        )
                        // Keep focus in the field.
                        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                        .on_click(cx.listener(move |this, _, _, cx| this.remove_chip(index, cx)))
                        .child(icon("x", ICON_SIZE_SM - 4.0, foreground)),
                )
        });

        let suggestions = showing.then(|| {
            Popover::new().min_width(px(SUGGESTIONS_WIDTH)).children(
                self.suggestions
                    .iter()
                    .enumerate()
                    .map(|(index, suggestion)| {
                        let highlighted = index == self.highlighted;
                        let (background, foreground, muted) = if highlighted {
                            (
                                Some(colors.selection_background),
                                colors.selection_foreground,
                                colors.selection_foreground,
                            )
                        } else {
                            (None, colors.foreground, colors.muted)
                        };
                        let field_id = field_id.clone();
                        div()
                            .id(("recipient-suggestion", index))
                            .debug_selector(move || format!("suggestion-{field_id}-{index}"))
                            .flex()
                            .items_center()
                            .gap(px(SPACE_2))
                            .min_h(TextScale::get(cx).fit(MENU_ITEM_HEIGHT, TYPE_BODY))
                            .px(px(SPACE_2))
                            .rounded(px(RADIUS_SM))
                            .accessible(
                                Role::ListBoxOption,
                                SharedString::from(suggestion.to_header_value()),
                            )
                            .aria_selected(highlighted)
                            .cursor_pointer()
                            .when_some(background, |this, background| this.bg(rgb(background)))
                            .text_color(rgb(foreground))
                            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                if *hovered && this.highlighted != index {
                                    this.highlighted = index;
                                    cx.notify();
                                }
                            }))
                            // Pick on press, before the field could lose
                            // focus and turn the typed text into a chip.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    window.prevent_default();
                                    this.accept_suggestion(index, cx);
                                }),
                            )
                            .child(icon("user", ICON_SIZE_SM, muted))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(SPACE_2))
                                    .when_some(suggestion.name.clone(), |this, name| {
                                        this.child(
                                            div().type_style(TYPE_BODY).ellipsis().child(name),
                                        )
                                    })
                                    .child(
                                        div()
                                            .type_style(TYPE_CAPTION)
                                            .text_color(rgb(muted))
                                            .ellipsis()
                                            .child(suggestion.email.clone()),
                                    ),
                            )
                    }),
            )
        });

        let mut key_context = KeyContext::default();
        key_context.add(KEY_CONTEXT);
        if showing {
            key_context.add(SUGGESTIONS_CONTEXT);
        }
        let input = self.input.clone();
        div()
            .id(SharedString::from(format!("recipients-{field_id}")))
            .key_context(key_context)
            .capture_action(cx.listener(Self::backspace))
            .capture_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::cancel))
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(SPACE_1))
            .p(px(SPACE_0_5))
            .rounded(px(RADIUS_MD))
            .accessible(Role::List, self.label.clone())
            .cursor_text()
            // A click on the empty part of the field types into it.
            .on_click(move |_, window, cx| {
                input.update(cx, |input, cx| input.focus(window, cx));
            })
            .children(chips)
            .child(
                div()
                    .flex_1()
                    .min_w(rems_from_px(INPUT_MIN_WIDTH))
                    .child(self.input.clone()),
            )
            .children(suggestions.map(popover_below))
            .children(showing.then(|| {
                let highlight = highlight_announcement(
                    &self
                        .suggestions
                        .get(self.highlighted)
                        .map(|suggestion| suggestion.to_header_value())
                        .unwrap_or_default(),
                    self.highlighted,
                    self.suggestions.len(),
                );
                live_region(
                    (
                        "recipient-suggestion-highlight",
                        announcement_key(&highlight),
                    ),
                    highlight,
                    Politeness::Polite,
                )
            }))
    }
}
