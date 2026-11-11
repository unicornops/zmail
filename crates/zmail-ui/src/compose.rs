//! Compose: the draft editor, in the detail pane or a window of its own.
//!
//! [`ComposeView`] is its own entity with its own focus handle and the
//! `Compose` key context. It edits one draft at a time: the shell opens a
//! draft with [`ComposeView::load_draft`], edits autosave through
//! [`MailActions`] once typing pauses for [`AUTOSAVE_DELAY`], and Send
//! queues the draft with the sync service. Status and errors go to whoever
//! hosts it as [`ComposeEvent`]s.
//!
//! New mail opens in a [`ComposeWindow`] (see [`window`]), one window per
//! draft; the detail pane hosts replies and drafts picked from the Drafts
//! list, and can pop a draft out into its own window. A draft is only ever
//! open in one place.
//!
//! Attachments come from the system file picker (`prompt_for_paths`) or
//! files dropped onto the editor (`ExternalPaths`), and show as cards with
//! their size and a Remove button. They are copied in on a background
//! thread ([`attaching`]): Send waits for them, and past
//! [`attaching::LARGE_ATTACHMENTS_BYTES`] in all a banner warns that the
//! message may be too large for some providers.
//!
//! Send checks the draft first ([`send_checks`]): with no recipients, or
//! one that is not a valid address, it says what to fix in a banner above
//! the fields and focuses the field at fault; with no subject, or a
//! message that mentions an attachment with nothing attached, it asks
//! before sending.
//!
//! Discard deletes the draft, after a confirmation with Cancel focused
//! unless the draft is empty; the host closes the editor on
//! [`ComposeEvent::Discarded`]. A compose window closed on an empty draft
//! drops it ([`ComposeView::drop_if_empty`]), so New Message never leaves
//! an empty draft behind.
//!
//! [`ComposeState`] holds the open draft's inputs and store-owned fields;
//! [`recipients`] has the To, Cc and Bcc chip fields.
//! The inputs are built once per draft and never rebuilt from the store
//! while that draft stays open, so saving and the shell's store refreshes
//! never move the cursor.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, FontWeight, IntoElement, PathPromptOptions, Render, Role, SharedString,
    StatefulInteractiveElement, Subscription, Task, Window, div, prelude::*, px, rgb,
};
use zmail_core::{DraftAttachment, DraftDetail, DraftState, DraftUpdate, IdentitySummary};
use zmail_store::LocalStore;
use zmail_sync::SyncService;

use crate::a11y::AccessibleExt;
use crate::actions::{
    AddAttachment, DiscardDraft, RemoveAttachment, SaveDraft, SelectIdentity, Send, ShowBcc, ShowCc,
};
use crate::components::{
    Button, ButtonSize, HitTargetExt, Modal, ModalState, Select, SelectEvent, TextInput,
    TextInputEvent,
};
use crate::mail_actions::MailActions;
use crate::pane::{dispatch_from, pane_focus_indicator};
use crate::styles::{
    ICON_BUTTON_SIZE, ICON_SIZE_SM, PlatformStyle, RADIUS_LG, SPACE_3, SendButtonPlacement,
    TYPE_BODY, TypeStyleExt, UiColors, rems_from_px,
};
use crate::widgets::{EllipsisExt, empty_state, format_size, icon, label_chip, status_banner};

mod attaching;
mod recipients;
pub(crate) mod send_checks;
pub(crate) mod window;

pub(crate) use recipients::{RecipientField, RecipientFieldEvent};
pub(crate) use window::ComposeWindow;

use send_checks::SendCheck;

/// The key context the compose pane sets.
pub(crate) const KEY_CONTEXT: &str = "Compose";
/// Height of a header field row (From, To, Cc, Bcc, Subject).
const FIELD_ROW_HEIGHT: f32 = 40.0;
/// Width of the header field labels, so the fields line up.
const FIELD_LABEL_WIDTH: f32 = 56.0;
/// Width of an attachment card.
const ATTACHMENT_CARD_WIDTH: f32 = 240.0;
/// How long typing must pause before the draft autosaves.
pub(crate) use zmail_actions::AUTOSAVE_DELAY;

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
/// refresh never move the cursor or drop a chip.
pub(crate) struct ComposeState {
    pub(crate) draft_id: i64,
    pub(crate) account_id: i64,
    pub(crate) identity_id: i64,
    pub(crate) identity_email: String,
    /// The reply-to the draft is sent with (its identity's), shown under
    /// From; empty for none.
    pub(crate) identity_reply_to: String,
    pub(crate) to: Entity<RecipientField>,
    pub(crate) cc: Entity<RecipientField>,
    pub(crate) bcc: Entity<RecipientField>,
    /// Whether the Cc and Bcc fields show. They hide behind toggles until
    /// used, and show whenever the draft has recipients there.
    pub(crate) show_cc: bool,
    pub(crate) show_bcc: bool,
    pub(crate) subject: Entity<TextInput>,
    pub(crate) body: Entity<TextInput>,
    pub(crate) attachments: Vec<DraftAttachment>,
    pub(crate) state: DraftState,
    pub(crate) last_error_message: Option<String>,
}

impl ComposeState {
    /// Construct a [`ComposeState`] from a store-fetched [`DraftDetail`],
    /// creating its inputs.
    pub(crate) fn from_draft(
        draft: DraftDetail,
        presentation: Presentation,
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
            identity_reply_to: draft.identity_reply_to,
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
                    .rows(
                        if presentation == Presentation::Inline {
                            6
                        } else {
                            12
                        },
                        40,
                    )
                    .label("Message")
                    .placeholder("Write your message…")
                    .with_text(draft.body)
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
        self.identity_reply_to = draft.identity_reply_to;
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
    /// The draft was queued for sending. It leaves after `undo_delay`
    /// (the undo-send window); until then the outbox job `job_id` can be
    /// cancelled.
    Queued {
        draft_id: i64,
        account_id: i64,
        job_id: i64,
        undo_delay: Duration,
    },
    /// The pane's Open in Window: move this draft into its own window.
    PopOut { draft_id: i64 },
    /// The pane's Show Window, for a draft already open in a window.
    ShowWindow { draft_id: i64 },
    /// The draft was discarded (deleted); the editor is empty now.
    Discarded { draft_id: i64 },
}

/// Where a [`ComposeView`] is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Presentation {
    /// The detail pane of the main window, under a pane header.
    Pane,
    /// A compose window, whose title bar holds the title and, on macOS and
    /// Linux, Send.
    Window,
    /// The inline reply at the bottom of the open conversation: a card that
    /// grows with its text, without the title or Subject, with Send below
    /// and a button that pops the reply out into its own window.
    Inline,
}

pub(crate) struct ComposeView {
    focus_handle: FocusHandle,
    store: Arc<LocalStore>,
    sync_service: SyncService,
    mail_actions: Entity<MailActions>,
    presentation: Presentation,
    /// The draft's account's identities, for the From choice.
    identities: Vec<IdentitySummary>,
    /// The From menu, when the account has more than one identity.
    from_menu: Option<(Entity<Select>, Subscription)>,
    pub(crate) compose: Option<ComposeState>,
    /// In the pane: the selected draft, which is open in its own window
    /// instead.
    in_window: Option<i64>,
    /// Subscriptions to the open draft's inputs.
    input_subscriptions: Vec<Subscription>,
    /// Set by an edit that has not been saved yet.
    unsaved_edits: bool,
    /// The pending autosave. Each edit replaces it, which cancels the one
    /// before, so the save runs once typing pauses.
    autosave_task: Task<()>,
    /// Why the last Send did not go: a plain sentence saying what to fix,
    /// shown above the fields until it is fixed.
    send_problem: Option<String>,
    /// The question Send is asking ("Send without a subject?"), its
    /// dialog, and the dialog's Cancel, which has focus when it opens.
    send_check: Option<SendCheck>,
    send_check_modal: ModalState,
    send_check_cancel: FocusHandle,
    /// The questions answered Send Anyway during this press of Send.
    waived_checks: Vec<SendCheck>,
    /// Attachments being copied into the open draft, and those that could
    /// not be ([`attaching`]).
    pending_attachments: Vec<attaching::PendingAttachment>,
    next_pending_attachment: usize,
    /// The "Discard this draft?" confirmation, and its Cancel button, which
    /// has focus when it opens.
    discard_modal: ModalState,
    discard_cancel: FocusHandle,
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
        presentation: Presentation,
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
            presentation,
            identities: Vec::new(),
            from_menu: None,
            compose: None,
            in_window: None,
            input_subscriptions: Vec::new(),
            unsaved_edits: false,
            autosave_task: Task::ready(()),
            send_problem: None,
            send_check: None,
            send_check_modal: ModalState::new(cx),
            send_check_cancel: cx.focus_handle(),
            waived_checks: Vec::new(),
            pending_attachments: Vec::new(),
            next_pending_attachment: 0,
            discard_modal: ModalState::new(cx),
            discard_cancel: cx.focus_handle(),
            _quit_subscription: quit_subscription,
        }
    }

    /// What the banner says about the last Send, if anything.
    #[cfg(test)]
    pub(crate) fn send_problem(&self) -> Option<&str> {
        self.send_problem.as_deref()
    }

    /// The question Send is asking, if any.
    #[cfg(test)]
    pub(crate) fn send_check(&self) -> Option<SendCheck> {
        self.send_check
    }

    /// Whether the discard confirmation shows, and whether its Cancel has
    /// focus.
    #[cfg(test)]
    pub(crate) fn discard_confirmation(&self, window: &Window) -> Option<bool> {
        self.discard_modal
            .is_open()
            .then(|| self.discard_cancel.is_focused(window))
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

    /// In the pane: the draft shown as open in its own window.
    pub(crate) fn showing_in_window(&self) -> Option<i64> {
        self.in_window
    }

    /// The draft's title: its subject, or "New Message".
    pub(crate) fn title(&self, cx: &App) -> String {
        self.compose
            .as_ref()
            .map(|compose| compose.subject.read(cx).text().trim().to_owned())
            .filter(|subject| !subject.is_empty())
            .unwrap_or_else(|| "New Message".to_owned())
    }

    /// Loads the identities of the draft's account, for From.
    fn load_identities(&mut self, account_id: i64, cx: &mut Context<Self>) {
        let identities = self.store.list_identities(account_id).unwrap_or_default();
        if self.identities != identities {
            self.identities = identities;
            self.from_menu = None;
        }
        self.sync_from_menu(cx);
    }

    /// Saves and closes the open draft, leaving the editor empty.
    pub(crate) fn close_draft(&mut self, cx: &mut Context<Self>) {
        self.flush_autosave(cx);
        self.compose = None;
        self.send_problem = None;
        self.pending_attachments.clear();
        self.in_window = None;
        self.input_subscriptions.clear();
        self.from_menu = None;
        cx.notify();
    }

    /// Whether the open draft has nothing in it: no recipients, subject,
    /// text or attachments.
    fn is_empty(&self, cx: &App) -> bool {
        self.compose.as_ref().is_some_and(|compose| {
            compose.attachments.is_empty()
                && compose
                    .recipient_fields()
                    .iter()
                    .all(|field| field.read(cx).recipients(cx).is_empty())
                && compose
                    .draft_inputs()
                    .iter()
                    .all(|input| input.read(cx).text().trim().is_empty())
        })
    }

    /// Discard: asks first, with Cancel focused, unless the draft is
    /// empty, which goes at once.
    pub(crate) fn request_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.compose.is_none() {
            return;
        }
        if self.is_empty(cx) {
            self.discard(window, cx);
            return;
        }
        let cancel = self.discard_cancel.clone();
        self.discard_modal.open_focusing(&cancel, window, cx);
        cx.notify();
    }

    fn discard_draft(&mut self, _: &DiscardDraft, window: &mut Window, cx: &mut Context<Self>) {
        self.request_discard(window, cx);
    }

    fn close_discard_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.discard_modal.close(window, cx);
        cx.notify();
    }

    /// Deletes the open draft, edits not saved yet included, and empties
    /// the editor.
    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_discard_dialog(window, cx);
        let Some(draft_id) = self.draft_id() else {
            return;
        };
        if let Err(error) = self.delete_open_draft(draft_id, cx) {
            cx.emit(ComposeEvent::Error(error.to_string()));
            return;
        }
        cx.emit(ComposeEvent::Discarded { draft_id });
    }

    /// When the open draft is empty and still being written, deletes it
    /// and returns `true`: closing an empty compose window leaves nothing
    /// behind in Drafts. Otherwise saves any edits, as closing does.
    pub(crate) fn drop_if_empty(&mut self, cx: &mut Context<Self>) -> bool {
        let empty = self.is_empty(cx)
            && self
                .compose
                .as_ref()
                .is_some_and(|compose| compose.state == DraftState::Editing);
        let Some(draft_id) = self.draft_id().filter(|_| empty) else {
            self.flush_autosave(cx);
            return false;
        };
        match self.delete_open_draft(draft_id, cx) {
            Ok(()) => true,
            Err(error) => {
                cx.emit(ComposeEvent::Error(error.to_string()));
                false
            }
        }
    }

    /// Deletes the open draft through [`MailActions`], dropping any
    /// pending autosave, and empties the editor.
    fn delete_open_draft(&mut self, draft_id: i64, cx: &mut Context<Self>) -> Result<()> {
        self.mail_actions
            .update(cx, |actions, cx| actions.delete_draft(draft_id, cx))?;
        self.unsaved_edits = false;
        self.autosave_task = Task::ready(());
        self.compose = None;
        self.in_window = None;
        self.input_subscriptions.clear();
        self.from_menu = None;
        cx.notify();
        Ok(())
    }

    /// In the pane: shows that `draft_id` is open in its own window, with a
    /// button to bring that window forward, instead of editing it here.
    pub(crate) fn show_in_window(&mut self, draft_id: i64, cx: &mut Context<Self>) {
        self.close_draft(cx);
        self.in_window = Some(draft_id);
        cx.notify();
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
        self.in_window = None;
        let account_id = draft.account_id;
        match self.compose.as_mut() {
            Some(compose) if compose.draft_id == draft_id => compose.update_from_draft(draft),
            _ => {
                let compose =
                    ComposeState::from_draft(draft, self.presentation, &self.store, window, cx);
                self.input_subscriptions = Self::watch_inputs(&compose, window, cx);
                self.compose = Some(compose);
                self.send_problem = None;
                self.pending_attachments.clear();
            }
        }
        self.load_identities(account_id, cx);
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
            let account_id = draft.account_id;
            compose.update_from_draft(draft);
            self.load_identities(account_id, cx);
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

    /// Subscribes to the inputs: edits autosave the draft.
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
                    RecipientFieldEvent::Changed => {
                        this.recheck_recipients(cx);
                        this.schedule_autosave(window, cx);
                    }
                },
            )
        }));
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

    fn send(&mut self, _: &Send, window: &mut Window, cx: &mut Context<Self>) {
        self.waived_checks.clear();
        self.send_draft(window, cx);
    }

    /// Saves the draft and queues it to send once the undo-send window
    /// ([`UiPreferences::undo_send_delay`]) has passed, after the checks
    /// of [`send_checks`]: a problem with the recipients stops it, with a
    /// banner saying what to fix and focus in the field at fault; an empty
    /// subject or a missing attachment asks first.
    ///
    /// [`UiPreferences::undo_send_delay`]: zmail_core::preferences::UiPreferences::undo_send_delay
    pub(crate) fn send_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((draft_id, account_id, state)) = self
            .compose
            .as_ref()
            .map(|compose| (compose.draft_id, compose.account_id, compose.state))
        else {
            cx.emit(ComposeEvent::Error("No draft is open.".to_owned()));
            return;
        };
        let already_gone = match state {
            DraftState::Queued | DraftState::Sending => Some(send_checks::ALREADY_QUEUED),
            DraftState::Sent => Some(send_checks::ALREADY_SENT),
            DraftState::Editing | DraftState::Failed => None,
        };
        if let Some(problem) = already_gone.or_else(|| {
            self.attaching()
                .then_some(send_checks::WAIT_FOR_ATTACHMENTS)
        }) {
            self.send_problem = Some(problem.to_owned());
            cx.notify();
            return;
        }
        if let Some((field, problem)) = self.recipient_problem(cx) {
            self.send_problem = Some(problem);
            self.focus_recipient_field(&field, window, cx);
            cx.notify();
            return;
        }
        self.send_problem = None;
        if let Some(check) = self.unanswered_check(cx) {
            self.send_check = Some(check);
            let cancel = self.send_check_cancel.clone();
            self.send_check_modal.open_focusing(&cancel, window, cx);
            cx.notify();
            return;
        }
        self.waived_checks.clear();
        if let Err(error) = self.save(cx) {
            cx.emit(ComposeEvent::Error(error.to_string()));
            return;
        }
        let undo_delay = self
            .store
            .ui_preferences()
            .map(|preferences| preferences.undo_send_delay())
            .unwrap_or_default();
        match self
            .sync_service
            .queue_draft_and_send_after(draft_id, undo_delay)
        {
            Ok(job_id) => cx.emit(ComposeEvent::Queued {
                draft_id,
                account_id,
                job_id,
                undo_delay,
            }),
            Err(error) => cx.emit(ComposeEvent::Error(format!(
                "Couldn't queue this message to send: {error}"
            ))),
        }
    }

    /// What stops the draft from sending as its recipients stand: the
    /// field at fault and what to say. Recipients that are not valid
    /// addresses come first (focus goes to the first field with one);
    /// otherwise a draft with nobody to send to.
    fn recipient_problem(&self, cx: &App) -> Option<(Entity<RecipientField>, String)> {
        let compose = self.compose.as_ref()?;
        let mut invalid = Vec::new();
        let mut first_at_fault = None;
        for field in compose.recipient_fields() {
            let field_invalid = field.read(cx).invalid_recipients(cx);
            if !field_invalid.is_empty() {
                first_at_fault.get_or_insert_with(|| field.clone());
            }
            let label = field.read(cx).label().to_string();
            invalid.extend(
                field_invalid
                    .into_iter()
                    .map(|recipient| (label.clone(), recipient)),
            );
        }
        if let Some(field) = first_at_fault {
            return Some((field, send_checks::invalid_recipients_message(&invalid)));
        }
        compose
            .recipient_fields()
            .iter()
            .all(|field| field.read(cx).recipients(cx).is_empty())
            .then(|| (compose.to.clone(), send_checks::NO_RECIPIENTS.to_owned()))
    }

    /// While the banner shows a recipient problem, edits to the recipients
    /// update it, and it goes once they are fixed.
    fn recheck_recipients(&mut self, cx: &mut Context<Self>) {
        if self.send_problem.is_some() {
            self.send_problem = self.recipient_problem(cx).map(|(_, problem)| problem);
            cx.notify();
        }
    }

    /// Focuses a recipient field, showing Cc or Bcc first if hidden.
    fn focus_recipient_field(
        &mut self,
        field: &Entity<RecipientField>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(compose) = self.compose.as_mut() {
            if *field == compose.cc {
                compose.show_cc = true;
            } else if *field == compose.bcc {
                compose.show_bcc = true;
            }
        }
        field.update(cx, |field, cx| field.focus(window, cx));
    }

    /// The first question Send still has to ask about this draft: no
    /// subject (not for the inline reply, which keeps the conversation's),
    /// then a mentioned attachment that is not attached.
    fn unanswered_check(&self, cx: &App) -> Option<SendCheck> {
        let compose = self.compose.as_ref()?;
        let no_subject = self.presentation != Presentation::Inline
            && compose.subject.read(cx).text().trim().is_empty();
        let no_attachment = compose.attachments.is_empty()
            && send_checks::mentions_attachment(compose.body.read(cx).text());
        [
            (no_subject, SendCheck::NoSubject),
            (no_attachment, SendCheck::NoAttachment),
        ]
        .into_iter()
        .find(|(applies, check)| *applies && !self.waived_checks.contains(check))
        .map(|(_, check)| check)
    }

    /// Send Anyway: sends unless another question is still to come.
    fn send_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(check) = self.send_check.take() {
            self.waived_checks.push(check);
        }
        self.send_check_modal.close(window, cx);
        self.send_draft(window, cx);
    }

    /// Cancel: back to the draft, in the field the question was about.
    fn cancel_send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let check = self.send_check.take();
        self.waived_checks.clear();
        self.send_check_modal.close(window, cx);
        if let Some(compose) = self.compose.as_ref() {
            match check {
                Some(SendCheck::NoSubject) => focus_input(&compose.subject, window, cx),
                Some(SendCheck::NoAttachment) => focus_input(&compose.body, window, cx),
                None => {}
            }
        }
        cx.notify();
    }

    /// Attach Files…: picks files with the system file picker.
    fn add_attachment(&mut self, _: &AddAttachment, window: &mut Window, cx: &mut Context<Self>) {
        if self.compose.is_none() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match picked.await {
                Ok(Ok(Some(paths))) => paths,
                // Cancelled, or the picker went away.
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |_, cx| {
                        cx.emit(ComposeEvent::Error(format!(
                            "The file picker could not open: {error}"
                        )));
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| this.attach_paths(paths, cx));
        })
        .detach();
    }

    /// Files dropped onto the editor.
    fn on_drop_paths(&mut self, paths: &ExternalPaths, _: &mut Window, cx: &mut Context<Self>) {
        self.attach_paths(paths.paths().to_vec(), cx);
    }

    /// Copies each file into the draft as an attachment, in the
    /// background ([`attaching`]). Folders and unreadable files stay as
    /// cards that say why.
    pub(crate) fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.compose.is_none() {
            return;
        }
        if let Err(error) = self.save(cx) {
            cx.emit(ComposeEvent::Error(error.to_string()));
            return;
        }
        self.attach_files(paths, cx);
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

/// Send, disabled while attachments are being copied in, with the reason
/// as its tooltip.
pub(crate) fn send_button(id: &'static str, attaching: bool) -> Button {
    let button = Button::new(id, "Send")
        .primary()
        .icon("send")
        .shortcut("secondary-enter")
        .disabled(attaching);
    if attaching {
        button.tooltip("Send is available once the attachments have finished copying")
    } else {
        button
    }
}

impl ComposeView {
    /// "Discard this draft?", with Cancel focused (see
    /// [`ComposeView::request_discard`]).
    fn render_discard_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.discard_modal.is_open() {
            return None;
        }
        Some(
            Modal::new("discard-draft-dialog", self.discard_modal.focus_handle())
                .title("Discard this draft?")
                .description(
                    "The draft and its attachments are deleted from Zmail. This can't be undone.",
                )
                .footer(
                    div()
                        .debug_selector(|| "discard-draft-cancel".into())
                        .child(
                            Button::new("discard-draft-cancel", "Cancel")
                                .track_focus(&self.discard_cancel)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_discard_dialog(window, cx)
                                })),
                        ),
                )
                .footer(
                    div()
                        .debug_selector(|| "discard-draft-confirm".into())
                        .child(
                            Button::new("discard-draft-confirm", "Discard Draft")
                                .danger()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.discard(window, cx)),
                                ),
                        ),
                )
                .on_dismiss(
                    cx.listener(|this, _, window, cx| this.close_discard_dialog(window, cx)),
                )
                .into_any_element(),
        )
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
        let draft_open = self.compose.is_some();
        let pane = self.presentation == Presentation::Pane;
        let inline = self.presentation == Presentation::Inline;
        // In a window, Send sits in the title bar unless the platform puts
        // it at the bottom of the compose body (Windows).
        let bottom_send = self.presentation != Presentation::Window
            || PlatformStyle::current().send_button == SendButtonPlacement::ComposeBottomLeading;

        div()
            .id("compose")
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .accessible(Role::Form, self.title(cx))
            .on_action(cx.listener(Self::send))
            .on_action(cx.listener(Self::save_draft))
            .on_action(cx.listener(Self::discard_draft))
            .on_action(cx.listener(Self::add_attachment))
            .on_action(cx.listener(Self::remove_attachment))
            .on_action(cx.listener(Self::select_identity))
            .on_action(cx.listener(Self::show_cc))
            .on_action(cx.listener(Self::show_bcc))
            .when(draft_open, |this| {
                this.on_drop(cx.listener(Self::on_drop_paths))
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.bg(rgb(colors.selected_background))
                    })
            })
            .flex()
            .flex_col()
            .bg(rgb(colors.background))
            // Inline, the card sizes to its content: the conversation list
            // around it scrolls.
            .when(!inline, |this| this.size_full())
            .when(inline, |this| {
                this.w_full()
                    .rounded(px(RADIUS_LG))
                    .border_1()
                    .border_color(rgb(if focused {
                        colors.focus_ring
                    } else {
                        colors.border
                    }))
            })
            .when(pane, |this| {
                this.child(
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
            })
            .child(if inline {
                div()
                    .px_4()
                    .pt_2()
                    .child(self.render_detail(&colors, cx))
                    .into_any_element()
            } else {
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
                    .child(self.render_detail(&colors, cx))
                    .into_any_element()
            })
            .when(draft_open && bottom_send, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .when(!inline, |this| {
                            this.border_t_1().border_color(rgb(colors.border))
                        })
                        .children(self.render_action_buttons(cx)),
                )
            })
            .children(self.render_send_check(cx))
            .children(self.render_discard_dialog(cx))
    }
}

impl ComposeView {
    /// The question Send is asking, with Cancel focused (see
    /// [`ComposeView::send_draft`]).
    fn render_send_check(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let check = self
            .send_check
            .filter(|_| self.send_check_modal.is_open())?;
        Some(
            Modal::new("send-check-dialog", self.send_check_modal.focus_handle())
                .title(check.title())
                .description(check.description())
                .footer(
                    div().debug_selector(|| "send-check-cancel".into()).child(
                        Button::new("send-check-cancel", "Cancel")
                            .track_focus(&self.send_check_cancel)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel_send(window, cx)),
                            ),
                    ),
                )
                .footer(
                    div().debug_selector(|| "send-check-confirm".into()).child(
                        Button::new("send-check-confirm", "Send Anyway")
                            .primary()
                            .icon("send")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_anyway(window, cx)),
                            ),
                    ),
                )
                .on_dismiss(cx.listener(|this, _, window, cx| this.cancel_send(window, cx)))
                .into_any_element(),
        )
    }
}

impl ComposeView {
    /// The form, or what the pane shows with no draft open.
    fn render_detail(&self, colors: &UiColors, cx: &mut Context<Self>) -> AnyElement {
        let Some(compose) = self.compose.as_ref() else {
            if let Some(draft_id) = self.in_window {
                return div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(empty_state(
                        "Open in its own window",
                        "This draft is being edited in a compose window.",
                        "pen-square",
                        None,
                        colors,
                    ))
                    .child(
                        Button::new("compose-show-window", "Show Window").on_click(cx.listener(
                            move |_, _, _, cx| cx.emit(ComposeEvent::ShowWindow { draft_id }),
                        )),
                    )
                    .into_any_element();
            }
            return empty_state(
                "No draft open",
                "Press Compose or select a draft to edit it here.",
                "pen-square",
                None,
                colors,
            );
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
        // A window shows the title in its title bar.
        let title = (self.presentation == Presentation::Pane).then(|| self.title(cx));

        div()
            .flex()
            .flex_col()
            .gap_4()
            .w_full()
            .min_w_0()
            .when(title.is_some() || state_chip.is_some(), |this| {
                this.child(
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
                                .children(title),
                        )
                        .children(state_chip),
                )
            })
            .when_some(self.send_problem.as_ref(), |this, problem| {
                this.child(status_banner(
                    "compose-send-problem",
                    Role::Alert,
                    problem,
                    "alert-circle",
                    colors.danger,
                    colors.danger_background,
                    colors,
                ))
            })
            .when_some(compose.last_error_message.as_ref(), |this, error| {
                this.child(status_banner(
                    "compose-send-error",
                    Role::Alert,
                    &format!("Last send error: {error}"),
                    "alert-circle",
                    colors.danger,
                    colors.danger_background,
                    colors,
                ))
            })
            .child(render_fields(
                compose,
                self.from_menu.as_ref().map(|(menu, _)| menu),
                self.presentation != Presentation::Inline,
                &self.focus_handle,
                colors,
            ))
            .when(!compose.attachments.is_empty(), |this| {
                this.child(render_attachments(
                    &compose.attachments,
                    &self.focus_handle,
                    colors,
                ))
            })
            .when(!self.pending_attachments.is_empty(), |this| {
                this.child(
                    div().flex().flex_wrap().gap_2().children(
                        self.pending_attachments
                            .iter()
                            .map(|pending| self.render_pending_card(pending, colors, cx)),
                    ),
                )
            })
            .when_some(self.large_attachments_notice(), |this, notice| {
                this.child(status_banner(
                    "compose-large-attachments",
                    Role::Status,
                    &notice,
                    "triangle-alert",
                    colors.warning,
                    colors.warning_background,
                    colors,
                ))
            })
            .into_any_element()
    }

    /// The buttons along the bottom: Send first (bottom-left, the Windows
    /// placement), then Attach; the pane adds Save Draft, then Discard, and
    /// the pane and the inline reply end with Open in Window.
    fn render_action_buttons(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let focus_handle = &self.focus_handle;
        let mut buttons = vec![
            send_button("compose-send", self.attaching())
                .on_click(dispatch_from(focus_handle, Send))
                .into_any_element(),
            Button::new("compose-attach", "Attach")
                .icon("paperclip")
                .tooltip("Attach files")
                .on_click(dispatch_from(focus_handle, AddAttachment))
                .into_any_element(),
        ];
        if self.presentation == Presentation::Pane {
            buttons.push(
                Button::new("compose-save", "Save Draft")
                    .icon("file-text")
                    .on_click(dispatch_from(focus_handle, SaveDraft))
                    .into_any_element(),
            );
        }
        buttons.push(
            div()
                .debug_selector(|| "compose-discard".into())
                .child(
                    Button::new("compose-discard", "Discard")
                        .ghost()
                        .icon("trash-2")
                        .tooltip("Discard this draft")
                        .on_click(dispatch_from(focus_handle, DiscardDraft)),
                )
                .into_any_element(),
        );
        if self.presentation != Presentation::Window
            && let Some(draft_id) = self.draft_id()
        {
            buttons.push(div().flex_1().into_any_element());
            buttons.push(
                div()
                    .debug_selector(|| "compose-pop-out".into())
                    .child(
                        Button::new("compose-pop-out", "Open in Window")
                            .ghost()
                            .icon("external-link")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(ComposeEvent::PopOut { draft_id })
                            })),
                    )
                    .into_any_element(),
            );
        }
        buttons
    }
}

/// The header fields, one row each, then the body: From (only when the
/// account has more than one identity), where replies go when the identity
/// has a reply-to (read-only), To with the Cc and Bcc toggles, Cc and Bcc
/// once shown, and Subject (not in the inline reply, which keeps the
/// conversation's).
fn render_fields(
    compose: &ComposeState,
    from_menu: Option<&Entity<Select>>,
    show_subject: bool,
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
                    .w(rems_from_px(FIELD_LABEL_WIDTH))
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
        .when(!compose.identity_reply_to.trim().is_empty(), |this| {
            this.child(render_reply_to(&compose.identity_reply_to, colors))
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
        .when(show_subject, |this| {
            this.child(
                row("Subject").child(div().flex_1().min_w_0().child(compose.subject.clone())),
            )
        })
        .child(div().pt(px(SPACE_3)).child(compose.body.clone()))
}

/// Where replies go: the identity's reply-to, which the message is sent
/// with as `Reply-To`. Read-only; it changes with the identity (edited in
/// Settings), and lines up with the fields.
fn render_reply_to(reply_to: &str, colors: &UiColors) -> gpui::Div {
    let text = format!("Replies go to: {}", reply_to.trim());
    div()
        .debug_selector(|| "compose-reply-to".into())
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .min_h(px(FIELD_ROW_HEIGHT))
        .border_b_1()
        .border_color(rgb(colors.border))
        .child(div().flex_none().w(rems_from_px(FIELD_LABEL_WIDTH)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .type_style(TYPE_BODY)
                .text_color(rgb(colors.muted))
                .ellipsis()
                .child(text),
        )
}

/// The draft's attachments, as cards with a Remove button.
fn render_attachments(
    attachments: &[DraftAttachment],
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> gpui::Div {
    div().flex().flex_wrap().gap_2().children(
        attachments
            .iter()
            .map(|attachment| render_attachment_card(attachment, focus_handle, colors)),
    )
}

fn render_attachment_card(
    attachment: &DraftAttachment,
    focus_handle: &FocusHandle,
    colors: &UiColors,
) -> gpui::Div {
    let attachment_id = attachment.id;

    div()
        .w(rems_from_px(ATTACHMENT_CARD_WIDTH))
        .max_w_full()
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
                        .hit_target(move || format!("compose-attachment-remove-{attachment_id}"))
                        .accessible(
                            Role::Button,
                            SharedString::from(format!("Remove {}", attachment.filename)),
                        )
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
