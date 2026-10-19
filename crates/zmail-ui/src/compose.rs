//! The compose pane: the open draft.
//!
//! [`ComposeView`] is its own entity with its own focus handle and the
//! `Compose` key context. It edits one draft at a time: the shell opens a
//! draft with [`ComposeView::load_draft`], edits autosave through
//! [`MailActions`] once typing pauses for [`AUTOSAVE_DELAY`], and Send
//! queues the draft with the sync service. Status and errors go to the
//! shell as [`ComposeEvent`]s.
//!
//! [`ComposeState`] holds the open draft's inputs and store-owned fields;
//! [`recipients`] has the To, Cc and Bcc chip fields.
//! The inputs are built once per draft and never rebuilt from the store
//! while that draft stays open, so saving and the shell's store refreshes
//! never move the cursor.

use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, IntoElement, Render, StatefulInteractiveElement, Subscription, Task, Window, div,
    prelude::*, px, rgb,
};
use zmail_core::{DraftAttachment, DraftDetail, DraftState, DraftUpdate, IdentitySummary};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::actions::{
    AddAttachment, RemoveAttachment, SaveDraft, SelectIdentity, Send, ShowBcc, ShowCc,
};
use crate::components::{Button, ButtonSize, Select, SelectEvent, TextInput, TextInputEvent};
use crate::mail_actions::MailActions;
use crate::pane::{dispatch_from, pane_focus_indicator};

mod recipients;

pub(crate) use recipients::{RecipientField, RecipientFieldEvent};

/// The key context the compose pane sets.
pub(crate) const KEY_CONTEXT: &str = "Compose";
/// Height of a header field row (From, To, Cc, Bcc, Subject).
const FIELD_ROW_HEIGHT: f32 = 40.0;
/// Width of the header field labels, so the fields line up.
const FIELD_LABEL_WIDTH: f32 = 56.0;
/// How long typing must pause before the draft autosaves.
pub(crate) const AUTOSAVE_DELAY: Duration = Duration::from_secs(1);
use crate::styles::{ICON_BUTTON_SIZE, ICON_SIZE_SM, SPACE_3, TYPE_BODY, TypeStyleExt, UiColors};
use crate::widgets::{
    EllipsisExt, empty_state, format_size, icon, input_card, label_chip, section_card,
    status_banner, toolbar_button,
};

// ---------------------------------------------------------------------------
// ComposeState
// ---------------------------------------------------------------------------

/// In-memory form state for a draft being actively composed.
///
/// The text fields are [`TextInput`] entities that own the text the user is
/// editing, with its cursor, selection and undo history, and the To, Cc
/// and Bcc fields are [`RecipientField`]s holding the recipient chips.
/// Store refreshes only update the store-owned fields
/// ([`ComposeState::update_from_draft`]), so autosave and the background
/// refresh never move the cursor, drop a chip or clear a half-typed
/// attachment path.
pub(crate) struct ComposeState {
    pub(crate) draft_id: i64,
    pub(crate) account_id: i64,
    pub(crate) identity_id: i64,
    pub(crate) identity_email: String,
    pub(crate) to: Entity<RecipientField>,
    pub(crate) cc: Entity<RecipientField>,
    pub(crate) bcc: Entity<RecipientField>,
    /// Whether the Cc and Bcc fields show. They hide behind toggles until
    /// used, and show whenever the draft has recipients there.
    pub(crate) show_cc: bool,
    pub(crate) show_bcc: bool,
    pub(crate) subject: Entity<TextInput>,
    pub(crate) body: Entity<TextInput>,
    pub(crate) attachment_path: Entity<TextInput>,
    pub(crate) attachments: Vec<DraftAttachment>,
    pub(crate) state: DraftState,
    pub(crate) last_error_message: Option<String>,
}

impl ComposeState {
    /// Construct a [`ComposeState`] from a store-fetched [`DraftDetail`],
    /// creating its inputs.
    pub(crate) fn from_draft(
        draft: DraftDetail,
        store: &Arc<LocalStore>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let recipients =
            |label: &'static str, values: &[String], window: &mut Window, cx: &mut App| {
                cx.new(|cx| RecipientField::new(store.clone(), label, values, window, cx))
            };
        Self {
            draft_id: draft.id,
            account_id: draft.account_id,
            identity_id: draft.identity_id,
            identity_email: draft.identity_email,
            to: recipients("To", &draft.to, window, cx),
            cc: recipients("Cc", &draft.cc, window, cx),
            bcc: recipients("Bcc", &draft.bcc, window, cx),
            show_cc: !draft.cc.is_empty(),
            show_bcc: !draft.bcc.is_empty(),
            subject: cx.new(|cx| {
                TextInput::new(cx)
                    .borderless()
                    .label("Subject")
                    .placeholder("Subject")
                    .with_text(draft.subject)
            }),
            body: cx.new(|cx| {
                TextInput::new(cx)
                    .borderless()
                    .multiline()
                    .rows(12, 40)
                    .label("Message")
                    .placeholder("Write your message…")
                    .with_text(draft.body)
            }),
            attachment_path: cx.new(|cx| {
                TextInput::new(cx)
                    .label("Attachment Path")
                    .placeholder("/path/to/attachment.pdf")
            }),
            attachments: draft.attachments,
            state: draft.state,
            last_error_message: draft.last_error_message,
        }
    }

    /// Refresh the store-owned fields (identity, attachments, send state)
    /// from the store without touching the text being edited.
    pub(crate) fn update_from_draft(&mut self, draft: DraftDetail) {
        self.identity_id = draft.identity_id;
        self.identity_email = draft.identity_email;
        self.attachments = draft.attachments;
        self.state = draft.state;
        self.last_error_message = draft.last_error_message;
    }

    /// The text inputs whose text is part of the saved draft.
    pub(crate) fn draft_inputs(&self) -> [&Entity<TextInput>; 2] {
        [&self.subject, &self.body]
    }

    /// The recipient fields.
    pub(crate) fn recipient_fields(&self) -> [&Entity<RecipientField>; 3] {
        [&self.to, &self.cc, &self.bcc]
    }
}

// ---------------------------------------------------------------------------
// ComposeView
// ---------------------------------------------------------------------------

/// What the compose pane reports to the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ComposeEvent {
    /// A short confirmation, for example "Draft saved.".
    Status(String),
    /// Something failed.
    Error(String),
    /// The draft was queued for sending.
    Queued { account_id: i64 },
}

pub(crate) struct ComposeView {
    focus_handle: FocusHandle,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    mail_actions: Entity<MailActions>,
    /// The selected account's identities, for the From choice.
    identities: Vec<IdentitySummary>,
    /// The From menu, when the account has more than one identity.
    from_menu: Option<(Entity<Select>, Subscription)>,
    pub(crate) compose: Option<ComposeState>,
    /// Subscriptions to the open draft's inputs.
    input_subscriptions: Vec<Subscription>,
    /// Set by an edit that has not been saved yet.
    unsaved_edits: bool,
    /// The pending autosave. Each edit replaces it, which cancels the one
    /// before, so the save runs once typing pauses.
    autosave_task: Task<()>,
    _quit_subscription: Subscription,
}

impl EventEmitter<ComposeEvent> for ComposeView {}

impl Focusable for ComposeView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ComposeView {
    pub(crate) fn new(
        store: Arc<LocalStore>,
        sync_service: SyncService,
        mail_actions: Entity<MailActions>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Quitting within the autosave delay of the last keystroke still
        // keeps it.
        let quit_subscription = cx.on_app_quit(|this, cx| {
            this.flush_autosave(cx);
            async {}
        });
        Self {
            focus_handle: cx.focus_handle(),
            store,
            sync_service,
            mail_actions,
            identities: Vec::new(),
            from_menu: None,
            compose: None,
            input_subscriptions: Vec::new(),
            unsaved_edits: false,
            autosave_task: Task::ready(()),
            _quit_subscription: quit_subscription,
        }
    }

    /// Whether the From menu shows.
    #[cfg(test)]
    pub(crate) fn has_from_menu(&self) -> bool {
        self.from_menu.is_some()
    }

    /// The open draft's id.
    pub(crate) fn draft_id(&self) -> Option<i64> {
        self.compose.as_ref().map(|compose| compose.draft_id)
    }

    /// Replaces the identities offered in From.
    pub(crate) fn set_identities(
        &mut self,
        identities: Vec<IdentitySummary>,
        cx: &mut Context<Self>,
    ) {
        if self.identities != identities {
            self.identities = identities;
            self.from_menu = None;
            self.sync_from_menu(cx);
            cx.notify();
        }
    }

    /// Builds the From menu when there is more than one identity to choose
    /// from, and keeps it on the draft's identity.
    fn sync_from_menu(&mut self, cx: &mut Context<Self>) {
        if self.identities.len() <= 1 {
            self.from_menu = None;
            return;
        }
        let selected = self.compose.as_ref().and_then(|compose| {
            self.identities
                .iter()
                .position(|identity| identity.id == compose.identity_id)
        });
        if let Some((menu, _)) = self.from_menu.as_ref() {
            if menu.read(cx).selected_index() != selected {
                menu.update(cx, |menu, cx| menu.set_selected(selected, cx));
            }
            return;
        }
        let options = self
            .identities
            .iter()
            .map(identity_label)
            .collect::<Vec<_>>();
        let menu = cx.new(|cx| Select::new(options, selected, cx).label("From"));
        let subscription = cx.subscribe(&menu, |this, _, event: &SelectEvent, cx| {
            let SelectEvent::Changed(index) = *event;
            if let Some(identity_id) = this.identities.get(index).map(|identity| identity.id) {
                this.choose_identity(identity_id, cx);
            }
        });
        self.from_menu = Some((menu, subscription));
    }

    /// Opens a draft. Re-opening the draft that is already open keeps its
    /// inputs, so the cursor stays put; opening another one first saves any
    /// edits to the open draft still waiting for the autosave.
    pub(crate) fn load_draft(
        &mut self,
        draft_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.draft_id().is_some_and(|open| open != draft_id) {
            self.flush_autosave(cx);
        }
        let draft = self
            .store
            .draft_detail(draft_id)?
            .context("This draft no longer exists.")?;
        match self.compose.as_mut() {
            Some(compose) if compose.draft_id == draft_id => compose.update_from_draft(draft),
            _ => {
                let compose = ComposeState::from_draft(draft, &self.store, window, cx);
                self.input_subscriptions = Self::watch_inputs(&compose, window, cx);
                self.compose = Some(compose);
            }
        }
        self.sync_from_menu(cx);
        cx.notify();
        Ok(())
    }

    /// Refreshes the store-owned parts of the open draft (identity,
    /// attachments, send state). The text inputs keep the user's text,
    /// cursor and undo history.
    pub(crate) fn refresh_from_store(&mut self, cx: &mut Context<Self>) {
        if let Some(compose) = self.compose.as_mut()
            && let Ok(Some(draft)) = self.store.draft_detail(compose.draft_id)
        {
            compose.update_from_draft(draft);
            self.sync_from_menu(cx);
            cx.notify();
        }
    }

    /// Focuses the To field.
    pub(crate) fn focus_to(&self, window: &mut Window, cx: &mut App) {
        if let Some(compose) = self.compose.as_ref() {
            compose.to.update(cx, |field, cx| field.focus(window, cx));
        }
    }

    /// Focuses the body with the cursor at the start, above any quoted
    /// message.
    pub(crate) fn focus_body_start(&self, window: &mut Window, cx: &mut App) {
        if let Some(compose) = self.compose.as_ref() {
            compose
                .body
                .update(cx, |body, cx| body.select_range(0..0, cx));
            focus_input(&compose.body, window, cx);
        }
    }

    /// Where focus goes when the user moves into the pane: the To field, or
    /// the pane itself when no draft is open.
    pub(crate) fn focus_pane(&self, window: &mut Window, cx: &mut App) {
        if self.compose.is_some() {
            self.focus_to(window, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Subscribes to the inputs: edits autosave the draft, and Enter in the
    /// attachment path adds the attachment.
    fn watch_inputs(
        compose: &ComposeState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let mut subscriptions: Vec<Subscription> = compose
            .draft_inputs()
            .into_iter()
            .map(|input| {
                cx.subscribe_in(
                    input,
                    window,
                    |this: &mut Self, _, event: &TextInputEvent, window, cx| {
                        if *event == TextInputEvent::Changed {
                            this.schedule_autosave(window, cx);
                        }
                    },
                )
            })
            .collect();
        subscriptions.extend(compose.recipient_fields().into_iter().map(|field| {
            cx.subscribe_in(
                field,
                window,
                |this: &mut Self, _, event: &RecipientFieldEvent, window, cx| match event {
                    RecipientFieldEvent::Changed => this.schedule_autosave(window, cx),
                },
            )
        }));
        subscriptions.push(cx.subscribe(
            &compose.attachment_path,
            |this: &mut Self, _, event: &TextInputEvent, cx| {
                if *event == TextInputEvent::Submitted {
                    this.add_attachment_from_input(cx);
                }
            },
        ));
        subscriptions
    }

    /// Writes the inputs back to the store through [`MailActions`].
    fn save(&mut self, cx: &mut Context<Self>) -> Result<()> {
        self.unsaved_edits = false;
        let Some(compose) = self.compose.as_ref() else {
            return Ok(());
        };
        let text = |input: &Entity<TextInput>| input.read(cx).text().to_owned();
        let recipients = |field: &Entity<RecipientField>| field.read(cx).recipients(cx);
        let update = DraftUpdate {
            identity_id: compose.identity_id,
            to: recipients(&compose.to),
            cc: recipients(&compose.cc),
            bcc: recipients(&compose.bcc),
            subject: text(&compose.subject),
            body: text(&compose.body),
        };
        let draft_id = compose.draft_id;
        self.mail_actions
            .update(cx, |actions, cx| actions.save_draft(draft_id, &update, cx))
    }

    /// Saves [`AUTOSAVE_DELAY`] after the latest edit. The shell refreshes
    /// the drafts list a moment later from the `DraftSaved` event.
    fn schedule_autosave(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unsaved_edits = true;
        self.autosave_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.flush_autosave(cx));
        });
        // The title follows the subject.
        cx.notify();
    }

    /// Saves edits still waiting for the autosave now. The pending timer
    /// then finds nothing to save.
    pub(crate) fn flush_autosave(&mut self, cx: &mut Context<Self>) {
        if !self.unsaved_edits {
            return;
        }
        if let Err(error) = self.save(cx) {
            cx.emit(ComposeEvent::Error(error.to_string()));
        }
    }

    fn save_draft(&mut self, _: &SaveDraft, _: &mut Window, cx: &mut Context<Self>) {
        if self.compose.is_none() {
            return;
        }
        match self.save(cx) {
            Ok(()) => cx.emit(ComposeEvent::Status("Draft saved.".to_owned())),
            Err(error) => cx.emit(ComposeEvent::Error(error.to_string())),
        }
    }

    fn send(&mut self, _: &Send, _: &mut Window, cx: &mut Context<Self>) {
        let Some((draft_id, account_id)) = self
            .compose
            .as_ref()
            .map(|compose| (compose.draft_id, compose.account_id))
        else {
            cx.emit(ComposeEvent::Error("No draft is open.".to_owned()));
            return;
        };
        if let Err(error) = self.save(cx) {
            cx.emit(ComposeEvent::Error(error.to_string()));
            return;
        }
        match self.sync_service.queue_draft_and_send(draft_id) {
            Ok(_job_id) => {
                let _ = self.sync_service.process_outbox(Some(account_id));
                cx.emit(ComposeEvent::Status(
                    "Message queued for sending.".to_owned(),
                ));
                cx.emit(ComposeEvent::Queued { account_id });
            }
            Err(error) => cx.emit(ComposeEvent::Error(error.to_string())),
        }
    }

    fn add_attachment(&mut self, _: &AddAttachment, _: &mut Window, cx: &mut Context<Self>) {
        self.add_attachment_from_input(cx);
    }

    /// Adds the file at the path in the attachment input.
    fn add_attachment_from_input(&mut self, cx: &mut Context<Self>) {
        let Some((draft_id, path_input)) = self
            .compose
            .as_ref()
            .map(|compose| (compose.draft_id, compose.attachment_path.clone()))
        else {
            cx.emit(ComposeEvent::Error("No draft is open.".to_owned()));
            return;
        };
        if let Err(error) = self.save(cx) {
            cx.emit(ComposeEvent::Error(error.to_string()));
            return;
        }
        let path = path_input.read(cx).text().trim().to_owned();
        if path.is_empty() {
            cx.emit(ComposeEvent::Error(
                "Enter an attachment path first.".to_owned(),
            ));
            return;
        }
        match self.mail_actions.update(cx, |actions, cx| {
            actions.add_draft_attachment(draft_id, &path, cx)
        }) {
            Ok(_) => {
                path_input.update(cx, |input, cx| input.set_text("", cx));
                self.refresh_from_store(cx);
            }
            Err(error) => cx.emit(ComposeEvent::Error(error.to_string())),
        }
    }

    fn remove_attachment(
        &mut self,
        action: &RemoveAttachment,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft_id) = self.draft_id() else {
            return;
        };
        let attachment_id = action.attachment_id;
        match self.mail_actions.update(cx, |actions, cx| {
            actions.remove_draft_attachment(draft_id, attachment_id, cx)
        }) {
            Ok(()) => self.refresh_from_store(cx),
            Err(error) => cx.emit(ComposeEvent::Error(error.to_string())),
        }
    }

    fn select_identity(&mut self, action: &SelectIdentity, _: &mut Window, cx: &mut Context<Self>) {
        self.choose_identity(action.identity_id, cx);
    }

    /// Sends the draft from another of the account's identities.
    fn choose_identity(&mut self, identity_id: i64, cx: &mut Context<Self>) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        compose.identity_id = identity_id;
        if let Some(identity) = self
            .identities
            .iter()
            .find(|identity| identity.id == identity_id)
        {
            compose.identity_email = identity.email_address.clone();
        }
        match self.save(cx) {
            Ok(()) => self.refresh_from_store(cx),
            Err(error) => cx.emit(ComposeEvent::Error(error.to_string())),
        }
        cx.notify();
    }

    fn show_cc(&mut self, _: &ShowCc, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(compose) = self.compose.as_mut() {
            compose.show_cc = true;
            compose.cc.update(cx, |field, cx| field.focus(window, cx));
            cx.notify();
        }
    }

    fn show_bcc(&mut self, _: &ShowBcc, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(compose) = self.compose.as_mut() {
            compose.show_bcc = true;
            compose.bcc.update(cx, |field, cx| field.focus(window, cx));
            cx.notify();
        }
    }
}

/// How an identity reads in the From menu.
fn identity_label(identity: &IdentitySummary) -> String {
    if identity.display_name.trim().is_empty() {
        identity.email_address.clone()
    } else {
        format!("{} <{}>", identity.display_name, identity.email_address)
    }
}

/// Moves keyboard focus to a text input.
fn focus_input(input: &Entity<TextInput>, window: &mut Window, cx: &mut App) {
    input.update(cx, |input, cx| input.focus(window, cx));
}

impl Render for ComposeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = *cx.global::<UiColors>();
        let focused = self.focus_handle.contains_focused(window, cx);

        div()
            .id("compose")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::save_draft))
            .on_action(cx.listener(Self::add_attachment))
            .on_action(cx.listener(Self::remove_attachment))
            .on_action(cx.listener(Self::select_identity))
            .on_action(cx.listener(Self::show_cc))
            .on_action(cx.listener(Self::show_bcc))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            .child(
                pane_focus_indicator(
                    div()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(rgb(colors.border)),
                    focused,
                    &colors,
                )
                .child("Compose"),
            )
            .child(
                div()
                    .id("compose-scroll")
                    .flex_1()
                    // A flex column stretches its content to the pane width,
                    // so fields and long text size to the pane.
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .px_4()
                    .py_4()
                    .child(render_compose_detail(
                        self.compose.as_ref(),
                        self.from_menu.as_ref().map(|(menu, _)| menu),
                        &self.focus_handle,
                        &colors,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(rgb(colors.border))
                    .children(compose_action_buttons(
                        self.compose.is_some(),
                        &self.focus_handle,
                        &colors,
                    )),
            )
    }
}

// ---------------------------------------------------------------------------
// render_compose_detail
// ---------------------------------------------------------------------------

/// Render the compose form: the header fields, body and attachments.
/// Returns an [`AnyElement`] ready to be placed inside a scrollable
/// container.
fn render_compose_detail(
    compose: Option<&ComposeState>,
    from_menu: Option<&Entity<Select>>,
    focus_handle: &FocusHandle,
    colors: &UiColors,
    cx: &App,
) -> AnyElement {
    let Some(compose) = compose else {
        return empty_state(
            "No draft open",
            "Press Compose or select a draft to edit it here.",
            "pen-square",
            None,
            colors,
        );
    };

    let subject = compose.subject.read(cx).text().trim().to_owned();
    let title = if subject.is_empty() {
        "New message".to_owned()
    } else {
        subject
    };
    // "Editing" is the normal state of an open draft; only call out the
    // states that need attention.
    let state_chip = (compose.state != DraftState::Editing).then(|| {
        label_chip(
            compose.state.label(),
            Some("file-text"),
            colors.button_background,
            colors.info,
        )
    });

    div()
        .flex()
        .flex_col()
        .gap_4()
        .w_full()
        .min_w_0()
        .child(
            div()
                .flex()
                .justify_between()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .child(title),
                )
                .children(state_chip),
        )
        .when_some(compose.last_error_message.as_ref(), |this, error| {
            this.child(status_banner(
                &format!("Last send error: {error}"),
                "alert-circle",
                colors.danger,
                colors.danger_background,
                colors,
            ))
        })
        .child(render_fields(compose, from_menu, focus_handle, colors))
        .child(render_attachments_section(compose, focus_handle, colors))
        .into_any_element()
}

// ---------------------------------------------------------------------------
// compose_action_buttons
// ---------------------------------------------------------------------------

/// The Send and Save Draft buttons at the bottom of the pane, so they stay
/// visible however long the draft is.
///
/// Returns an empty [`Vec`] when no draft is open.
fn compose_action_buttons(
    draft_open: bool,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> Vec<AnyElement> {
    if !draft_open {
        return Vec::new();
    }
    vec![
        toolbar_button(
            "Send",
            Some("send"),
            colors.accent,
            colors.accent,
            colors.background,
            dispatch_from(focus_handle, Send),
        )
        .into_any_element(),
        toolbar_button(
            "Save Draft",
            Some("file-text"),
            colors.button_background,
            colors.border,
            colors.foreground,
            dispatch_from(focus_handle, SaveDraft),
        )
        .into_any_element(),
    ]
}

/// The header fields, one row each, then the body: From (only when the
/// account has more than one identity), To with the Cc and Bcc toggles, Cc
/// and Bcc once shown, and Subject.
fn render_fields(
    compose: &ComposeState,
    from_menu: Option<&Entity<Select>>,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> gpui::Div {
    let row = |label: &'static str| {
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_3))
            .min_h(px(FIELD_ROW_HEIGHT))
            .border_b_1()
            .border_color(rgb(colors.border))
            .child(
                div()
                    .flex_none()
                    .w(px(FIELD_LABEL_WIDTH))
                    .type_style(TYPE_BODY)
                    .text_color(rgb(colors.muted))
                    .child(label),
            )
    };
    let toggle = |id: &'static str, label: &'static str, tooltip: &'static str| {
        Button::new(id, label)
            .ghost()
            .size(ButtonSize::Small)
            .tooltip(tooltip)
    };

    div()
        .flex()
        .flex_col()
        .w_full()
        .min_w_0()
        .when_some(from_menu, |this, menu| {
            this.child(row("From").child(div().flex_1().min_w_0().child(menu.clone())))
        })
        .child(
            row("To")
                .child(compose.to.clone())
                .when(!compose.show_cc, |this| {
                    this.child(
                        div().debug_selector(|| "compose-show-cc".into()).child(
                            toggle("compose-show-cc", "Cc", "Add Cc recipients")
                                .on_click(dispatch_from(focus_handle, ShowCc)),
                        ),
                    )
                })
                .when(!compose.show_bcc, |this| {
                    this.child(
                        div().debug_selector(|| "compose-show-bcc".into()).child(
                            toggle("compose-show-bcc", "Bcc", "Add Bcc recipients")
                                .on_click(dispatch_from(focus_handle, ShowBcc)),
                        ),
                    )
                }),
        )
        .when(compose.show_cc, |this| {
            this.child(row("Cc").child(compose.cc.clone()))
        })
        .when(compose.show_bcc, |this| {
            this.child(row("Bcc").child(compose.bcc.clone()))
        })
        .child(row("Subject").child(div().flex_1().min_w_0().child(compose.subject.clone())))
        .child(div().pt(px(SPACE_3)).child(compose.body.clone()))
}

fn render_attachments_section(
    compose: &ComposeState,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> gpui::Div {
    section_card("Attachments", colors)
        .child(input_card(
            "Attachment Path",
            &compose.attachment_path,
            colors,
        ))
        .child(div().flex().flex_wrap().gap_2().child(toolbar_button(
            "Add Attachment",
            Some("paperclip"),
            colors.button_background,
            colors.border,
            colors.foreground,
            dispatch_from(focus_handle, AddAttachment),
        )))
        .child(if compose.attachments.is_empty() {
            div()
                .rounded_md()
                .bg(rgb(colors.background))
                .border_1()
                .border_color(rgb(colors.border))
                .px_3()
                .py_3()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(icon("paperclip", ICON_SIZE_SM, colors.muted))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(colors.muted))
                                .child("No attachments added yet."),
                        ),
                )
        } else {
            div()
                .flex()
                .flex_col()
                .gap_2()
                .children(compose.attachments.iter().map(|attachment| {
                    render_attachment_card(attachment, focus_handle, colors).into_any_element()
                }))
        })
}

fn render_attachment_card(
    attachment: &DraftAttachment,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> gpui::Div {
    let attachment_id = attachment.id;

    div()
        .rounded_md()
        .bg(rgb(colors.background))
        .border_1()
        .border_color(rgb(colors.border))
        .px_3()
        .py_3()
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_start()
                        .gap_3()
                        .child(icon("file-text", ICON_SIZE_SM, colors.muted))
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_w_0()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .ellipsis()
                                        .child(attachment.filename.clone()),
                                )
                                .child(div().text_sm().text_color(rgb(colors.muted)).child(
                                    format!(
                                        "{} • {}",
                                        attachment.content_type,
                                        format_size(attachment.size_bytes)
                                    ),
                                )),
                        ),
                )
                .child(
                    div()
                        .id(("compose-attachment-remove", attachment.id as u64))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .w(px(ICON_BUTTON_SIZE))
                        .h(px(ICON_BUTTON_SIZE))
                        .rounded_full()
                        .bg(rgb(colors.button_background))
                        .border_1()
                        .border_color(rgb(colors.border))
                        .cursor_pointer()
                        .on_click(dispatch_from(
                            focus_handle,
                            RemoveAttachment { attachment_id },
                        ))
                        .child(icon("x", ICON_SIZE_SM, colors.muted)),
                ),
        )
}
