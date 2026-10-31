//! Writing mail, Pico-style (ADR 0007, sections 2 and 6): the Compose
//! screen's header fields (From, To, Cc, Bcc, Attchmnt, Subject) and its
//! body in a `ratatui-textarea` editor, with Pico's control keys.
//!
//! - **Drafts are the store's.** Compose edits one draft through
//!   [`zmail_actions::MailActions`], as the GUI does: it autosaves once
//!   typing pauses for [`AUTOSAVE_DELAY`], `^O` postpones it, `^C` gives it
//!   up (deleting it), and `^X` sends it with
//!   `SyncService::queue_draft_and_send_after`, so the undo-send delay and
//!   the outbox are the GUI's. Whichever Zmail holds the sync lock sends
//!   it.
//! - **Replies and forwards** start from [`zmail_actions::reply`], the
//!   GUI's own logic, on a copy of the message filtered for the terminal:
//!   quoted text goes through [`sanitize`] before it enters the editor, and
//!   a draft opened from the store is filtered the same way.
//! - **Attachments** are copied into the store's encrypted draft copies
//!   (`add_draft_attachment`), by a typed path with Tab completion or the
//!   `^T` file browser, and shown with their filtered names and sizes.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Style;
use ratatui_textarea::{CursorMove, DataCursor, Input, Key, TextArea, WrapMode};
use zmail_actions::{AUTOSAVE_DELAY, MailEvent, reply};
use zmail_core::{
    DraftAttachment, DraftDetail, DraftState, DraftUpdate, IdentitySummary, NewDraft,
    ThreadMessage,
    participants::{Participant, parse_address_list},
    text::count_noun,
    time::format_full_date,
};
use zmail_security::{
    attachment_safety::sanitize_filename,
    terminal::{sanitize, sanitize_line},
};

use crate::{
    app::{App, Prompt, Screen},
    message::size_label,
    paths::{BrowserEntry, PathInput, browse},
};

/// The largest file `^R` reads into the body.
const READ_FILE_LIMIT: u64 = 1024 * 1024;
/// Addresses Tab offers at once.
const COMPLETIONS: usize = 8;
/// Addresses the `^T` address book lists.
const ADDRESS_BOOK_SIZE: usize = 200;

/// The attach key the command bar shows: `^J`, which a Unix terminal in
/// raw mode tells apart from Enter, and `^A` on Windows, whose console may
/// not. Both keys always attach.
pub const ATTACH_KEY: &str = if cfg!(windows) { "^A" } else { "^J" };

/// A Compose field, in screen order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// The identity it is sent from (shown when the account has several).
    From,
    /// To.
    To,
    /// Cc.
    Cc,
    /// Bcc.
    Bcc,
    /// The attachments.
    Attachments,
    /// Subject.
    Subject,
    /// The message text.
    Body,
}

impl Field {
    /// The label in front of a header field, Pico's.
    pub fn label(self) -> &'static str {
        match self {
            Self::From => "From",
            Self::To => "To",
            Self::Cc => "Cc",
            Self::Bcc => "Bcc",
            Self::Attachments => "Attchmnt",
            Self::Subject => "Subject",
            Self::Body => "",
        }
    }

    /// Whether it holds addresses.
    pub fn is_address(self) -> bool {
        matches!(self, Self::To | Self::Cc | Self::Bcc)
    }
}

/// Pico's commands on the Compose screen, for the command bar and its
/// clicks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pico {
    /// `^X`: send, after asking.
    Send,
    /// `^O`: postpone (save the draft and leave).
    Postpone,
    /// `^C`: cancel, after asking when there are changes.
    Cancel,
    /// `^K`: cut the line (or the field's text, or the attachment).
    Cut,
    /// `^U`: put back what was cut.
    Uncut,
    /// `^J` (or `^A`): attach a file.
    Attach,
    /// `^G`: help.
    Help,
    /// `^R`: read a file into the body.
    ReadFile,
    /// `^W`: find text in the body.
    WhereIs,
    /// `^T`: the address book, or the file browser.
    Browse,
    /// Tab: complete the address.
    Complete,
    /// Change the identity sent from.
    NextFrom,
    /// `^_`: the message text in the external editor (opt-in).
    Editor,
}

impl Pico {
    /// The key that runs it, for a click on the command bar.
    pub fn key(self) -> KeyEvent {
        let control = |ch| KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
        match self {
            Self::Send => control('x'),
            Self::Postpone => control('o'),
            Self::Cancel => control('c'),
            Self::Cut => control('k'),
            Self::Uncut => control('u'),
            Self::Attach => control('j'),
            Self::Help => control('g'),
            Self::ReadFile => control('r'),
            Self::WhereIs => control('w'),
            Self::Browse => control('t'),
            Self::Complete => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            Self::NextFrom => KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            Self::Editor => control('_'),
        }
    }
}

/// What Tab found for the address being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The field.
    pub field: Field,
    /// Where the address being completed starts, in characters.
    pub start: usize,
    /// The addresses that match, filtered, best first.
    pub matches: Vec<String>,
    /// Which one is filled in.
    pub cycle: usize,
}

/// What the `^T` list picks from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerKind {
    /// Addresses from the address history, for an address field.
    Addresses(Field),
    /// A folder's files, to attach or (`read` set) to read into the body.
    Files {
        /// The folder listed.
        folder: PathBuf,
        /// Read the file into the body rather than attach it.
        read: bool,
    },
}

/// The `^T` list over the Compose screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// What it lists.
    pub kind: PickerKind,
    /// The entries: filtered labels, and the real path of a file.
    pub entries: Vec<BrowserEntry>,
    /// The entry the cursor is on.
    pub cursor: usize,
}

/// What a file prompt on the status line is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPurpose {
    /// Attach the file.
    Attach,
    /// Read the file into the body (`^R`).
    ReadFile,
}

/// A file prompt: the path typed, with Tab completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPrompt {
    /// What the file is for.
    pub purpose: PathPurpose,
    /// The path.
    pub field: PathInput,
}

impl PathPrompt {
    /// The question on the status line.
    pub fn label(&self) -> String {
        let question = match self.purpose {
            PathPurpose::Attach => "File to attach",
            PathPurpose::ReadFile => "File to insert into the text",
        };
        format!("{question}: {}", self.field.input)
    }
}

/// A question about the open draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    /// "Send message?"
    Send,
    /// "Cancel message?" (it is deleted).
    Cancel,
    /// "Delete this postponed message?"
    DeleteDraft(i64),
}

/// A message queued with the undo-send delay, which `U` can take back
/// until the delay ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingSend {
    /// The outbox job.
    pub job_id: i64,
    /// Its draft.
    pub draft_id: i64,
    /// When the delay ends.
    pub until: Instant,
}

/// The draft open on the Compose screen.
pub struct Compose {
    /// The draft.
    pub draft_id: i64,
    /// The account's identities, for From.
    pub identities: Vec<IdentitySummary>,
    /// The identity it is sent from.
    pub identity_id: i64,
    /// To, Cc, Bcc and Subject: one-line editors.
    pub to: TextArea<'static>,
    /// Cc.
    pub cc: TextArea<'static>,
    /// Bcc.
    pub bcc: TextArea<'static>,
    /// Subject.
    pub subject: TextArea<'static>,
    /// The message text.
    pub body: TextArea<'static>,
    /// The attachments, as the store holds them.
    pub attachments: Vec<DraftAttachment>,
    /// The attachment the cursor is on, in the Attchmnt field.
    pub attachment_cursor: usize,
    /// The field with the cursor.
    pub focus: Field,
    /// Compose created the draft (a new message, reply or forward), so
    /// cancelling it unchanged deletes it without asking; a postponed
    /// draft continued here stays.
    pub created: bool,
    /// The fields as they were opened, to tell whether anything changed.
    initial: (DraftUpdate, Vec<i64>),
    /// Edits not saved to the store yet.
    pub dirty: bool,
    /// When the last edit was made, for the autosave.
    pub last_edit: Instant,
    /// What `^K` cut, line by line, for `^U`.
    cut: Vec<String>,
    /// The last key was `^K`, so another one adds to the cut.
    cutting: bool,
    /// What Tab found for the address being typed.
    pub completion: Option<Completion>,
    /// The `^T` list, while it shows.
    pub picker: Option<Picker>,
}

/// A one-line field holding `text`, with the cursor at its end.
fn line_editor(text: &str) -> TextArea<'static> {
    let mut editor = TextArea::new(vec![text.to_owned()]);
    editor.set_cursor_line_style(Style::default());
    editor.move_cursor(CursorMove::End);
    editor
}

/// The message text's editor, holding `text`, with the cursor at the
/// top.
fn body_editor(text: &str) -> TextArea<'static> {
    let mut editor = TextArea::new(text.split('\n').map(str::to_owned).collect());
    editor.set_cursor_line_style(Style::default());
    editor.set_wrap_mode(WrapMode::WordOrGlyph);
    editor
}

/// The text of a one-line field.
fn line_text(editor: &TextArea<'_>) -> String {
    editor.lines().join(" ")
}

/// Every address in an address field, as address list entries: what the
/// GUI's recipient chips save.
pub fn recipients(text: &str) -> Vec<String> {
    parse_address_list(text)
        .into_iter()
        .filter(|participant| participant.email.is_some() || participant.name.is_some())
        .map(|participant| participant.to_header_value())
        .collect()
}

/// The entries of an address field that are not deliverable addresses:
/// the GUI's rule, one `@` with text on both sides, no spaces, and a
/// domain that neither starts nor ends with a dot nor has two in a row.
pub fn invalid_addresses(text: &str) -> Vec<String> {
    parse_address_list(text)
        .into_iter()
        .filter(|participant| participant.email.is_some() || participant.name.is_some())
        .filter(|participant| {
            let Some(email) = participant.email.as_deref() else {
                return true;
            };
            let Some((local, domain)) = email.split_once('@') else {
                return true;
            };
            local.is_empty()
                || domain.is_empty()
                || domain.contains('@')
                || email.contains(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | ','))
                || domain.starts_with('.')
                || domain.ends_with('.')
                || domain.contains("..")
        })
        .map(|participant| participant.to_header_value())
        .collect()
}

/// Where the address being typed at the end of `text` starts, in
/// characters: after the last comma or semicolon outside quotes and angle
/// brackets, and its spaces.
fn last_address_start(text: &str) -> usize {
    let mut in_quotes = false;
    let mut in_angle = false;
    let mut start = 0;
    for (index, ch) in text.chars().enumerate() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '<' if !in_quotes => in_angle = true,
            '>' if !in_quotes => in_angle = false,
            ',' | ';' if !in_quotes && !in_angle => start = index + 1,
            _ => {}
        }
    }
    start
        + text
            .chars()
            .skip(start)
            .take_while(|ch| ch.is_whitespace())
            .count()
}

/// An identity as From shows it, filtered (identities can come from the
/// server).
pub fn identity_label(identity: &IdentitySummary) -> String {
    let name = sanitize_line(&identity.display_name);
    let email = sanitize_line(&identity.email_address);
    if name.is_empty() {
        email
    } else {
        format!("{name} <{email}>")
    }
}

/// An attachment as the Attchmnt field shows it: its filtered name and
/// size.
pub fn attachment_label(attachment: &DraftAttachment) -> String {
    format!(
        "{} ({})",
        sanitize_filename(&sanitize_line(&attachment.filename)),
        size_label(attachment.size_bytes)
    )
}

/// `message` with everything a reply or forward copies from it filtered
/// for the terminal.
fn filtered(message: &ThreadMessage) -> ThreadMessage {
    ThreadMessage {
        sender: sanitize_line(&message.sender),
        recipients: sanitize_line(&message.recipients),
        subject: sanitize_line(&message.subject),
        rendered_body: sanitize(&message.rendered_body),
        ..message.clone()
    }
}

/// Crossterm's key as the editor's input, for the keys Compose leaves to
/// it (no control or Alt chords: those are Pico's, or nothing).
fn editor_input(key: KeyEvent) -> Option<Input> {
    let modifiers = key.modifiers;
    if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return None;
    }
    let key_code = match key.code {
        KeyCode::Char(ch) => Key::Char(ch),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => Key::Tab,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        _ => return None,
    };
    Some(Input {
        key: key_code,
        ctrl: false,
        alt: false,
        shift: false,
    })
}

impl Compose {
    /// Opens `draft`, filtering everything in it for the terminal: a draft
    /// started in the GUI quotes mail unfiltered.
    pub fn new(
        draft: DraftDetail,
        identities: Vec<IdentitySummary>,
        created: bool,
        focus: Field,
    ) -> Self {
        let addresses = |values: &[String]| {
            values
                .iter()
                .map(|value| sanitize_line(value))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut body = sanitize(&draft.body);
        // A new message starts with the signature alone: room to write
        // above it, as a reply has above its quote.
        if created && !body.is_empty() && !body.starts_with('\n') {
            body.insert_str(0, "\n\n");
        }
        let body = body_editor(&body);
        let mut compose = Self {
            draft_id: draft.id,
            identities,
            identity_id: draft.identity_id,
            to: line_editor(&addresses(&draft.to)),
            cc: line_editor(&addresses(&draft.cc)),
            bcc: line_editor(&addresses(&draft.bcc)),
            subject: line_editor(&sanitize_line(&draft.subject)),
            body,
            attachments: draft.attachments,
            attachment_cursor: 0,
            focus,
            created,
            // Replaced below, once the fields exist.
            initial: (
                DraftUpdate {
                    identity_id: draft.identity_id,
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: String::new(),
                    body: String::new(),
                },
                Vec::new(),
            ),
            dirty: false,
            last_edit: Instant::now(),
            cut: Vec::new(),
            cutting: false,
            completion: None,
            picker: None,
        };
        compose.initial = (compose.update(), compose.attachment_ids());
        compose
    }

    fn attachment_ids(&self) -> Vec<i64> {
        self.attachments
            .iter()
            .map(|attachment| attachment.id)
            .collect()
    }

    /// The fields as the store saves them.
    pub fn update(&self) -> DraftUpdate {
        DraftUpdate {
            identity_id: self.identity_id,
            to: recipients(&line_text(&self.to)),
            cc: recipients(&line_text(&self.cc)),
            bcc: recipients(&line_text(&self.bcc)),
            subject: line_text(&self.subject).trim().to_owned(),
            body: self.body.lines().join("\n"),
        }
    }

    /// Whether anything changed since the draft was opened.
    pub fn changed(&self) -> bool {
        self.update() != self.initial.0 || self.attachment_ids() != self.initial.1
    }

    /// The fields shown, in order: From only when the account has more
    /// than one identity, as in the GUI.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = Vec::with_capacity(7);
        if self.identities.len() > 1 {
            fields.push(Field::From);
        }
        fields.extend([
            Field::To,
            Field::Cc,
            Field::Bcc,
            Field::Attachments,
            Field::Subject,
            Field::Body,
        ]);
        fields
    }

    /// The editor of a text field.
    pub fn editor(&self, field: Field) -> Option<&TextArea<'static>> {
        match field {
            Field::To => Some(&self.to),
            Field::Cc => Some(&self.cc),
            Field::Bcc => Some(&self.bcc),
            Field::Subject => Some(&self.subject),
            Field::Body => Some(&self.body),
            Field::From | Field::Attachments => None,
        }
    }

    /// The editor of a text field, to change.
    pub fn editor_mut(&mut self, field: Field) -> Option<&mut TextArea<'static>> {
        match field {
            Field::To => Some(&mut self.to),
            Field::Cc => Some(&mut self.cc),
            Field::Bcc => Some(&mut self.bcc),
            Field::Subject => Some(&mut self.subject),
            Field::Body => Some(&mut self.body),
            Field::From | Field::Attachments => None,
        }
    }

    /// The text of a field.
    pub fn text(&self, field: Field) -> String {
        match field {
            Field::Body => self.body.lines().join("\n"),
            _ => self.editor(field).map(line_text).unwrap_or_default(),
        }
    }

    /// Replaces a one-line field's text, the cursor at its end.
    fn set_line(&mut self, field: Field, text: &str) {
        if let Some(editor) = self.editor_mut(field) {
            *editor = line_editor(text);
        }
    }

    /// Something changed: autosave once typing pauses.
    pub fn edited(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
    }

    /// Moves to the next field (`by` 1) or the one before (-1).
    pub fn move_focus(&mut self, by: isize) {
        let fields = self.fields();
        let position = fields
            .iter()
            .position(|field| *field == self.focus)
            .unwrap_or(0) as isize;
        let next = (position + by).clamp(0, fields.len() as isize - 1) as usize;
        self.focus = fields[next];
        if self.focus == Field::Body && by < 0 {
            self.body.move_cursor(CursorMove::Bottom);
        }
        self.completion = None;
    }

    /// Up and Down in the Attchmnt field go through the attachments, then
    /// on to the next field.
    fn attachments_step(&mut self, by: isize) {
        let next = self.attachment_cursor as isize + by;
        if next >= 0 && (next as usize) < self.attachments.len() {
            self.attachment_cursor = next as usize;
        } else {
            self.move_focus(by);
        }
    }

    /// The identity after the one sent from (`by` 1) or before it.
    fn cycle_identity(&mut self, by: isize) {
        let count = self.identities.len();
        if count < 2 {
            return;
        }
        let position = self
            .identities
            .iter()
            .position(|identity| identity.id == self.identity_id)
            .unwrap_or(0) as isize;
        self.identity_id = self.identities[(position + by).rem_euclid(count as isize) as usize].id;
        self.edited();
    }

    /// `^K`: cuts the body's line (and adds to the cut when the last key
    /// was `^K` too), or a header field's text.
    fn cut(&mut self) -> bool {
        let adding = self.cutting;
        if self.focus == Field::Body {
            let DataCursor(row, _) = self.body.cursor();
            let line = self.body.lines().get(row).cloned().unwrap_or_default();
            let last = row + 1 >= self.body.lines().len();
            self.body.move_cursor(CursorMove::Head);
            if last {
                self.body.delete_line_by_end();
            } else {
                self.body.start_selection();
                self.body.move_cursor(CursorMove::Down);
                self.body.cut();
            }
            if adding {
                self.cut.push(line);
            } else {
                self.cut = vec![line];
            }
        } else {
            let text = self.text(self.focus);
            if text.is_empty() {
                return false;
            }
            self.set_line(self.focus, "");
            self.cut = vec![text];
        }
        self.cutting = true;
        true
    }

    /// `^U`: puts back what was cut, at the cursor's line in the body.
    fn uncut(&mut self) -> bool {
        if self.cut.is_empty() {
            return false;
        }
        if self.focus == Field::Body {
            self.body.move_cursor(CursorMove::Head);
            self.body.insert_str(format!("{}\n", self.cut.join("\n")));
        } else {
            let text = self.cut.join(" ");
            if let Some(editor) = self.editor_mut(self.focus) {
                editor.insert_str(text);
            }
        }
        true
    }

    /// A paste, literally: into the body as it is (filtered), into an
    /// address field as one list (a line per address becomes a comma
    /// list), into Subject on one line.
    pub fn paste(&mut self, pasted: &str) -> bool {
        let text = match self.focus {
            Field::Body => sanitize(pasted),
            field if field.is_address() => {
                let addresses: Vec<String> = pasted
                    .split(['\n', '\r'])
                    .map(sanitize_line)
                    .filter(|line| !line.is_empty())
                    .collect();
                let mut text = addresses.join(", ");
                let before = self.text(field);
                if addresses.len() > 1
                    && !before.trim().is_empty()
                    && !before.trim_end().ends_with([',', ';'])
                {
                    text = format!(", {text}");
                }
                text
            }
            Field::Subject => sanitize_line(pasted),
            _ => return false,
        };
        let focus = self.focus;
        match self.editor_mut(focus) {
            Some(editor) => {
                editor.insert_str(text);
                self.completion = None;
                true
            }
            None => false,
        }
    }

    /// Finds `query` in the body after the cursor, wrapping round, ignoring
    /// case, and moves the cursor there.
    pub fn find(&mut self, query: &str) -> bool {
        let needle = query.to_lowercase();
        if needle.is_empty() {
            return false;
        }
        let lines: Vec<String> = self
            .body
            .lines()
            .iter()
            .map(|line| line.to_lowercase())
            .collect();
        let DataCursor(row, column) = self.body.cursor();
        let count = lines.len();
        for step in 0..=count {
            let line_index = (row + step) % count.max(1);
            let line = &lines[line_index];
            // On the cursor's line, after the cursor first, and before it
            // last.
            let from = if step == 0 {
                line.char_indices()
                    .nth(column + 1)
                    .map_or(line.len(), |(index, _)| index)
            } else {
                0
            };
            let found = line[from..].find(&needle).map(|at| at + from);
            if let Some(at) = found.filter(|at| step < count || *at < from.max(1)) {
                let column = line[..at].chars().count();
                self.body.move_cursor(CursorMove::Jump(
                    u16::try_from(line_index).unwrap_or(u16::MAX),
                    u16::try_from(column).unwrap_or(u16::MAX),
                ));
                self.focus = Field::Body;
                return true;
            }
        }
        false
    }
}

/// A callback that logs a change's events.
fn log(events: &mut Vec<MailEvent>) -> impl FnMut(MailEvent) + '_ {
    move |event| events.push(event)
}

impl App {
    // ------------------------------------------------------------------
    // Opening compose
    // ------------------------------------------------------------------

    /// The drafts that are being written (not queued, sending, sent or
    /// failed), newest first: Alpine's postponed messages.
    pub(crate) fn postponed_drafts(&self) -> Vec<zmail_core::DraftSummary> {
        self.store
            .list_drafts(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|draft| draft.state == DraftState::Editing)
            .collect()
    }

    /// `C`: asks to continue a postponed message when there are any, as
    /// Alpine does, or starts a new one.
    pub(crate) fn compose(&mut self) {
        if self.compose.is_some() {
            self.goto_compose();
            return;
        }
        if self.postponed_drafts().is_empty() {
            self.new_message();
        } else {
            self.prompt = Some(Prompt::Postponed);
        }
    }

    /// The account a new message is sent from: the current folder's, else
    /// the one under the index's cursor, else the first.
    fn compose_account(&self) -> Option<i64> {
        self.folder
            .as_ref()
            .and_then(|folder| folder.account_id)
            .or_else(|| {
                self.index
                    .entries
                    .get(self.index.cursor)
                    .map(|entry| entry.account_id)
            })
            .or_else(|| self.accounts.first().map(|account| account.id))
    }

    /// The account's identities and the one a new draft is sent from: the
    /// primary, as in the GUI.
    fn identities_of(&self, account_id: i64) -> anyhow::Result<(Vec<IdentitySummary>, i64)> {
        let identities = self.store.list_identities(account_id)?;
        let identity_id = identities
            .iter()
            .find(|identity| identity.is_primary)
            .or_else(|| identities.first())
            .map(|identity| identity.id)
            .ok_or_else(|| anyhow::anyhow!("This account has no address to send from yet"))?;
        Ok((identities, identity_id))
    }

    /// Starts a new message.
    pub(crate) fn new_message(&mut self) {
        let Some(account_id) = self.compose_account() else {
            self.status = Some("Add an account before writing mail".to_owned());
            return;
        };
        let result = self.identities_of(account_id).and_then(|(_, identity_id)| {
            self.actions.create_draft(
                &NewDraft {
                    account_id,
                    identity_id,
                    to: Vec::new(),
                    cc: Vec::new(),
                    bcc: Vec::new(),
                    subject: String::new(),
                    body: String::new(),
                },
                &mut |_| {},
            )
        });
        match result {
            Ok(draft_id) => self.open_compose(draft_id, true, Field::To),
            Err(error) => self.report(error),
        }
    }

    /// Opens draft `draft_id` on the Compose screen with the cursor in
    /// `focus`.
    pub(crate) fn open_compose(&mut self, draft_id: i64, created: bool, focus: Field) {
        let draft = match self.store.draft_detail(draft_id) {
            Ok(Some(draft)) => draft,
            Ok(None) => {
                self.status = Some("That draft is gone".to_owned());
                return;
            }
            Err(error) => return self.report(error),
        };
        let identities = self
            .store
            .list_identities(draft.account_id)
            .unwrap_or_default();
        let mut compose = Compose::new(draft, identities, created, focus);
        if focus == Field::Body {
            compose.body.move_cursor(CursorMove::Top);
        }
        self.compose = Some(compose);
        self.goto_compose();
    }

    /// Shows the Compose screen, remembering where to go back to.
    fn goto_compose(&mut self) {
        if !matches!(
            self.screen,
            Screen::Compose | Screen::Help | Screen::Postponed
        ) {
            self.compose_from = self.screen;
        }
        self.prompt = None;
        self.goto(Screen::Compose);
    }

    /// The message a reply or forward starts from: in Message Text, the
    /// newest one that starts on screen (or above it), so scrolling back
    /// to an older message replies to that one; in the index, a single
    /// message's row, else the conversation's newest.
    fn reply_source(&self) -> Option<(i64, i64, Option<i64>)> {
        match self.screen {
            Screen::Message => {
                let state = self.message.as_ref()?;
                let pane = self.pane(crate::pane::PaneId::Message);
                let bottom = pane.map(|pane| pane.scroll + pane.height().saturating_sub(1));
                let reading = bottom
                    .and_then(|bottom| {
                        state
                            .drawn_starts
                            .iter()
                            .rev()
                            .find(|(_, line)| *line <= bottom)
                    })
                    .map(|(id, _)| *id)
                    .or(state.focus);
                Some((
                    state.conversation.thread_id,
                    state.conversation.account_id,
                    reading,
                ))
            }
            Screen::Index => self
                .index
                .entries
                .get(self.index.cursor)
                .map(|entry| (entry.thread_id, entry.account_id, entry.message_id)),
            _ => None,
        }
    }

    /// The conversation and message of a reply or forward, filtered.
    fn reply_message(
        &self,
        thread_id: i64,
        message_id: Option<i64>,
    ) -> anyhow::Result<(String, ThreadMessage)> {
        let view = self
            .store
            .thread_view(thread_id)?
            .ok_or_else(|| anyhow::anyhow!("That conversation is gone"))?;
        let message = match message_id {
            Some(id) => view.messages.iter().find(|message| message.id == id),
            None => None,
        }
        .or_else(|| view.messages.last())
        .ok_or_else(|| anyhow::anyhow!("That conversation has no messages"))?;
        Ok((sanitize_line(&view.subject), filtered(message)))
    }

    /// `R`: replies, asking first whether to reply to all when that adds
    /// anyone, as Alpine does.
    pub(crate) fn reply(&mut self) {
        let Some((thread_id, account_id, message_id)) = self.reply_source() else {
            self.status = Some("No messages here".to_owned());
            return;
        };
        let asks = self
            .reply_message(thread_id, message_id)
            .ok()
            .and_then(|(_, message)| {
                let own = self.own_addresses(account_id);
                let one = reply::reply_recipients(&message, &own, false).ok()?;
                let all = reply::reply_recipients(&message, &own, true).ok()?;
                Some(one != all)
            })
            .unwrap_or(false);
        if asks {
            self.prompt = Some(Prompt::ReplyAll {
                thread_id,
                account_id,
                message_id,
            });
        } else {
            self.start_reply(thread_id, account_id, message_id, false);
        }
    }

    fn own_addresses(&self, account_id: i64) -> std::collections::HashSet<String> {
        reply::own_addresses(&self.store.list_identities(account_id).unwrap_or_default())
    }

    /// Creates the reply draft and opens it, the cursor at the start of the
    /// body, above the quote.
    pub(crate) fn start_reply(
        &mut self,
        thread_id: i64,
        account_id: i64,
        message_id: Option<i64>,
        reply_all: bool,
    ) {
        let zone = *self.now().offset();
        let result = self
            .reply_message(thread_id, message_id)
            .and_then(|(subject, message)| {
                let date = format_full_date(&message.received_at, &zone, self.hour_cycle);
                reply::reply(
                    &subject,
                    &message,
                    &self.own_addresses(account_id),
                    reply_all,
                    &date,
                )
            })
            .and_then(|start| self.create_started_draft(account_id, start));
        match result {
            Ok(draft_id) => self.open_compose(draft_id, true, Field::Body),
            Err(error) => self.report(error),
        }
    }

    /// `F`: forwards the message, the cursor in To.
    pub(crate) fn forward(&mut self) {
        let Some((thread_id, account_id, message_id)) = self.reply_source() else {
            self.status = Some("No messages here".to_owned());
            return;
        };
        let zone = *self.now().offset();
        let result = self
            .reply_message(thread_id, message_id)
            .map(|(subject, message)| {
                let date = format_full_date(&message.received_at, &zone, self.hour_cycle);
                reply::forward(&subject, &message, &date)
            })
            .and_then(|start| self.create_started_draft(account_id, start));
        match result {
            Ok(draft_id) => self.open_compose(draft_id, true, Field::To),
            Err(error) => self.report(error),
        }
    }

    fn create_started_draft(
        &mut self,
        account_id: i64,
        start: reply::DraftStart,
    ) -> anyhow::Result<i64> {
        let (_, identity_id) = self.identities_of(account_id)?;
        self.actions.create_draft(
            &NewDraft {
                account_id,
                identity_id,
                to: start.to,
                cc: start.cc,
                bcc: Vec::new(),
                subject: start.subject,
                body: start.body,
            },
            &mut |_| {},
        )
    }

    // ------------------------------------------------------------------
    // Keys
    // ------------------------------------------------------------------

    /// A key on the Compose screen, with no question asked.
    pub(crate) fn compose_key(&mut self, key: KeyEvent) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        if compose.picker.is_some() {
            return self.picker_key(key);
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let cutting = control && key.code == KeyCode::Char('k');
        if !cutting {
            compose.cutting = false;
        }
        if key.code != KeyCode::Tab && key.code != KeyCode::BackTab {
            compose.completion = None;
        }
        if control {
            let pico = match key.code {
                KeyCode::Char('x') => Pico::Send,
                KeyCode::Char('o') => Pico::Postpone,
                KeyCode::Char('c') => Pico::Cancel,
                KeyCode::Char('k') => Pico::Cut,
                KeyCode::Char('u') => Pico::Uncut,
                KeyCode::Char('j' | 'a') => Pico::Attach,
                KeyCode::Char('g') => Pico::Help,
                KeyCode::Char('r') => Pico::ReadFile,
                KeyCode::Char('w') => Pico::WhereIs,
                KeyCode::Char('t') => Pico::Browse,
                // ^_ arrives as Ctrl-7 from most terminals (0x1F), or as
                // Ctrl-_ or Ctrl-/ with keyboard enhancement.
                KeyCode::Char('_' | '7' | '/') => Pico::Editor,
                _ => return self.compose_motion(key),
            };
            return self.pico(pico);
        }
        let compose = self.compose.as_mut().expect("checked above");
        let focus = compose.focus;
        match (focus, key.code) {
            (Field::Body, KeyCode::Up) if compose.body.cursor().0 == 0 => {
                compose.move_focus(-1);
            }
            (Field::Body, _) => {
                if let Some(input) = editor_input(key)
                    && compose.body.input(input)
                {
                    compose.edited();
                }
            }
            (Field::From, KeyCode::Left) => compose.cycle_identity(-1),
            (Field::From, KeyCode::Right | KeyCode::Char(' ')) => compose.cycle_identity(1),
            (Field::Attachments, KeyCode::Up) => compose.attachments_step(-1),
            (Field::Attachments, KeyCode::Down) => compose.attachments_step(1),
            (Field::Attachments, KeyCode::Delete) => self.pico(Pico::Cut),
            (Field::Attachments, KeyCode::Enter) => self.pico(Pico::Attach),
            (Field::Attachments, KeyCode::Char(ch)) => {
                // Typing in Attchmnt starts the file prompt.
                self.prompt = Some(Prompt::Path(PathPrompt {
                    purpose: PathPurpose::Attach,
                    field: PathInput {
                        input: ch.to_string(),
                        ..PathInput::default()
                    },
                }));
            }
            (_, KeyCode::Up | KeyCode::BackTab) => compose.move_focus(-1),
            (_, KeyCode::Down | KeyCode::Enter) => compose.move_focus(1),
            (field, KeyCode::Tab) if field.is_address() => self.complete_address(true),
            (_, KeyCode::Tab) => compose.move_focus(1),
            (Field::From | Field::Attachments, _) => {}
            (field, _) => {
                if let Some(input) = editor_input(key)
                    && let Some(editor) = compose.editor_mut(field)
                    && editor.input(input)
                {
                    compose.edited();
                }
            }
        }
    }

    /// Pico's movement keys: `^F`/`^B` a character, `^P`/`^N` a line,
    /// `^E` the end of the line, `^V`/`^Y` a page, `^D` and `^H` delete.
    fn compose_motion(&mut self, key: KeyEvent) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let focus = compose.focus;
        let body = focus == Field::Body;
        let Some(editor) = compose.editor_mut(focus) else {
            return;
        };
        let changed = match key.code {
            KeyCode::Char('f') => {
                editor.move_cursor(CursorMove::Forward);
                false
            }
            KeyCode::Char('b') => {
                editor.move_cursor(CursorMove::Back);
                false
            }
            KeyCode::Char('e') => {
                editor.move_cursor(CursorMove::End);
                false
            }
            KeyCode::Char('d') => editor.delete_next_char(),
            KeyCode::Char('h') => editor.delete_char(),
            KeyCode::Char('p') if body => {
                if editor.cursor().0 == 0 {
                    compose.move_focus(-1);
                } else {
                    editor.move_cursor(CursorMove::Up);
                }
                false
            }
            KeyCode::Char('n') if body => {
                editor.move_cursor(CursorMove::Down);
                false
            }
            KeyCode::Char('v') if body => {
                editor.input(Input {
                    key: Key::PageDown,
                    ..Input::default()
                });
                false
            }
            KeyCode::Char('y') if body => {
                editor.input(Input {
                    key: Key::PageUp,
                    ..Input::default()
                });
                false
            }
            KeyCode::Char('p') => {
                compose.move_focus(-1);
                false
            }
            KeyCode::Char('n') => {
                compose.move_focus(1);
                false
            }
            _ => false,
        };
        if changed {
            compose.edited();
        }
    }

    /// Runs a Pico command.
    pub(crate) fn pico(&mut self, pico: Pico) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let focus = compose.focus;
        match pico {
            Pico::Send => self.prompt = Some(Prompt::Confirm(Confirm::Send)),
            Pico::Postpone => self.postpone(),
            Pico::Cancel => self.cancel_compose(),
            Pico::Cut if focus == Field::Attachments => self.remove_attachment(),
            Pico::Cut => {
                if compose.cut() {
                    compose.edited();
                }
            }
            Pico::Uncut => {
                if compose.uncut() {
                    compose.edited();
                } else {
                    self.status = Some("Nothing has been cut".to_owned());
                }
            }
            Pico::Attach => {
                self.prompt = Some(Prompt::Path(PathPrompt {
                    purpose: PathPurpose::Attach,
                    field: PathInput::default(),
                }));
            }
            Pico::Help => {
                self.help_topic = Some(COMPOSE_HELP_TOPIC);
                self.run(crate::app::Cmd::Help);
            }
            Pico::ReadFile => {
                self.prompt = Some(Prompt::Path(PathPrompt {
                    purpose: PathPurpose::ReadFile,
                    field: PathInput::default(),
                }));
            }
            Pico::WhereIs => {
                self.prompt = Some(Prompt::Text {
                    purpose: crate::app::TextPurpose::FindInBody,
                    input: String::new(),
                });
            }
            Pico::Browse if focus.is_address() => self.open_address_book(focus),
            Pico::Browse if focus == Field::Attachments => self.open_file_browser(None, false),
            Pico::Browse => {
                self.status =
                    Some("^T lists addresses in To, Cc and Bcc, and files in Attchmnt".to_owned());
            }
            Pico::Complete if focus.is_address() => self.complete_address(true),
            Pico::Complete => {}
            Pico::NextFrom => compose.cycle_identity(1),
            Pico::Editor => self.external_edit(),
        }
    }

    /// `^_`: hands the message text to `$VISUAL` or `$EDITOR`, when Setup
    /// allows it (off by default).
    fn external_edit(&mut self) {
        if !self.prefs.external_editor {
            self.status = Some(
                "The external editor is off. Setup turns it on (the draft is then written \
                 unencrypted to a temporary file while the editor is open)"
                    .to_owned(),
            );
            return;
        }
        let Some(command) = crate::editor::editor_command() else {
            self.status = Some("Set VISUAL or EDITOR to the editor to use".to_owned());
            return;
        };
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        compose.focus = Field::Body;
        let text = compose.text(Field::Body);
        self.effects
            .push(crate::app::Effect::Edit { command, text });
    }

    /// The external editor exited: its text replaces the body, or what
    /// went wrong shows on the status line.
    pub fn external_edit_done(&mut self, result: anyhow::Result<String>) {
        match result {
            Ok(text) => {
                if let Some(compose) = self.compose.as_mut() {
                    if compose.text(Field::Body) != text {
                        compose.body = body_editor(&text);
                        compose.edited();
                    }
                    compose.focus = Field::Body;
                }
                self.status = Some(
                    "Text from the editor. Its temporary file was overwritten and removed"
                        .to_owned(),
                );
            }
            Err(error) => self.report(error),
        }
    }

    /// A paste while the Compose screen has no question open.
    pub(crate) fn compose_paste(&mut self, pasted: &str) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        if compose.picker.is_some() {
            return;
        }
        if compose.focus == Field::Attachments {
            // A pasted path goes into the file prompt.
            self.prompt = Some(Prompt::Path(PathPrompt {
                purpose: PathPurpose::Attach,
                field: PathInput {
                    input: sanitize_line(pasted),
                    ..PathInput::default()
                },
            }));
            return;
        }
        if compose.paste(pasted) {
            compose.edited();
        } else {
            self.status = Some("Pasting works in the text fields".to_owned());
        }
    }

    // ------------------------------------------------------------------
    // Addresses
    // ------------------------------------------------------------------

    /// Tab in an address field: completes the address being typed from the
    /// address history (the GUI's suggestions, best first), then cycles
    /// through the matches.
    pub(crate) fn complete_address(&mut self, forward: bool) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let field = compose.focus;
        let text = compose.text(field);
        if let Some(completion) = compose.completion.as_mut()
            && completion.field == field
            && completion.matches.len() > 1
        {
            let count = completion.matches.len();
            completion.cycle = if forward {
                (completion.cycle + 1) % count
            } else {
                (completion.cycle + count - 1) % count
            };
            let start = completion.start;
            let chosen = completion.matches[completion.cycle].clone();
            let before: String = text.chars().take(start).collect();
            compose.set_line(field, &format!("{before}{chosen}"));
            compose.edited();
            return;
        }
        let start = last_address_start(&text);
        let typed: String = text.chars().skip(start).collect();
        let typed = typed.trim().to_owned();
        if typed.is_empty() {
            self.status = Some(
                "Type the start of a name or address, then Tab. ^T lists your addresses".to_owned(),
            );
            return;
        }
        let present: Vec<String> = parse_address_list(&text)
            .iter()
            .map(Participant::key)
            .collect();
        let matches: Vec<String> = self
            .store
            .address_suggestions(&typed, COMPLETIONS + present.len())
            .unwrap_or_default()
            .into_iter()
            .filter(|suggestion| !present.contains(&suggestion.email.to_ascii_lowercase()))
            .take(COMPLETIONS)
            .map(|suggestion| filtered_address(suggestion.name.as_deref(), &suggestion.email))
            .collect();
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let before: String = text.chars().take(start).collect();
        match matches.as_slice() {
            [] => {
                self.status = Some(format!(
                    "No address you've used starts with \"{}\"",
                    sanitize_line(&typed)
                ));
            }
            [only] => {
                compose.set_line(field, &format!("{before}{only}, "));
                compose.edited();
            }
            _ => {
                compose.set_line(field, &format!("{before}{}", matches[0]));
                compose.edited();
                compose.completion = Some(Completion {
                    field,
                    start,
                    matches,
                    cycle: 0,
                });
            }
        }
    }

    /// `^T` in an address field: the address history, most used first, to
    /// pick from.
    fn open_address_book(&mut self, field: Field) {
        let entries: Vec<BrowserEntry> = self
            .store
            .address_suggestions("", ADDRESS_BOOK_SIZE)
            .unwrap_or_default()
            .into_iter()
            .map(|suggestion| BrowserEntry {
                label: filtered_address(suggestion.name.as_deref(), &suggestion.email),
                path: PathBuf::new(),
                folder: false,
            })
            .collect();
        if entries.is_empty() {
            self.status =
                Some("No addresses yet: they come from the mail you send and receive".to_owned());
            return;
        }
        if let Some(compose) = self.compose.as_mut() {
            compose.picker = Some(Picker {
                kind: PickerKind::Addresses(field),
                entries,
                cursor: 0,
            });
        }
    }

    // ------------------------------------------------------------------
    // Files
    // ------------------------------------------------------------------

    /// `^T` on Attchmnt or in a file prompt: the file browser, in `folder`
    /// (the last one browsed, else the home folder).
    pub(crate) fn open_file_browser(&mut self, folder: Option<PathBuf>, read: bool) {
        let folder = folder
            .filter(|folder| folder.is_dir())
            .or_else(|| self.browse_folder.clone())
            .or_else(std::env::home_dir)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        match browse(&folder) {
            Ok(entries) => {
                self.browse_folder = Some(folder.clone());
                if let Some(compose) = self.compose.as_mut() {
                    compose.picker = Some(Picker {
                        kind: PickerKind::Files { folder, read },
                        entries,
                        cursor: 0,
                    });
                }
            }
            Err(error) => {
                self.status = Some(format!(
                    "Couldn't list {}: {}",
                    sanitize_line(&folder.display().to_string()),
                    sanitize_line(&error.to_string())
                ));
            }
        }
    }

    /// A key while the `^T` list shows.
    fn picker_key(&mut self, key: KeyEvent) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let Some(picker) = compose.picker.as_mut() else {
            return;
        };
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = picker.entries.len().saturating_sub(1);
        let page = self
            .panes
            .iter()
            .find(|pane| pane.id == crate::pane::PaneId::Picker)
            .map_or(10, |pane| pane.height().saturating_sub(3).max(1));
        picker.cursor = match key.code {
            KeyCode::Esc => {
                compose.picker = None;
                return;
            }
            KeyCode::Char('c' | 'g') if control => {
                compose.picker = None;
                return;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('>') => return self.pick(),
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('<') => {
                if let PickerKind::Files { folder, read } = &picker.kind
                    && let Some(parent) = folder.parent()
                {
                    let (parent, read) = (parent.to_path_buf(), *read);
                    self.open_file_browser(Some(parent), read);
                }
                return;
            }
            KeyCode::Up | KeyCode::Char('p' | 'P') => picker.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('n' | 'N') => (picker.cursor + 1).min(last),
            KeyCode::PageUp | KeyCode::Char('-') => picker.cursor.saturating_sub(page),
            KeyCode::PageDown | KeyCode::Char(' ') => (picker.cursor + page).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => return,
        };
        self.reveal.insert(crate::pane::PaneId::Picker);
    }

    /// Enter in the `^T` list: adds the address, opens the folder, or
    /// attaches (or reads in) the file.
    pub(crate) fn pick(&mut self) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let Some(picker) = compose.picker.clone() else {
            return;
        };
        let Some(entry) = picker.entries.get(picker.cursor).cloned() else {
            return;
        };
        match picker.kind {
            PickerKind::Addresses(field) => {
                compose.picker = None;
                compose.focus = field;
                let text = compose.text(field);
                let joined = if text.trim().is_empty() {
                    format!("{}, ", entry.label)
                } else if text.trim_end().ends_with([',', ';']) {
                    format!("{} {}, ", text.trim_end(), entry.label)
                } else {
                    format!("{}, {}, ", text.trim_end(), entry.label)
                };
                compose.set_line(field, &joined);
                compose.edited();
            }
            PickerKind::Files { read, .. } if entry.folder => {
                self.open_file_browser(Some(entry.path), read);
            }
            PickerKind::Files { read, .. } => {
                compose.picker = None;
                if read {
                    self.read_into_body(&entry.path);
                } else {
                    self.attach(&entry.path);
                }
            }
        }
    }

    /// Copies the file at `path` into the draft (encrypted, by the store).
    pub(crate) fn attach(&mut self, path: &Path) {
        let Some(compose) = self.compose.as_ref() else {
            return;
        };
        let draft_id = compose.draft_id;
        if path.is_dir() {
            self.status = Some(format!(
                "{} is a folder: attach the files in it",
                sanitize_line(&path.display().to_string())
            ));
            return;
        }
        if let Err(error) = self.save_compose() {
            return self.report(error);
        }
        match self
            .actions
            .add_draft_attachment(draft_id, path, &mut |_| {})
        {
            Ok(attachment) => {
                let label = attachment_label(&attachment);
                if let Some(parent) = path.parent().filter(|parent| parent.is_dir()) {
                    self.browse_folder = Some(parent.to_path_buf());
                }
                if let Some(compose) = self.compose.as_mut() {
                    compose.attachments.push(attachment);
                    compose.attachment_cursor = compose.attachments.len() - 1;
                    compose.focus = Field::Attachments;
                }
                self.status = Some(format!("Attached {label}"));
            }
            Err(error) => self.report(error),
        }
    }

    /// `^K` or Delete on Attchmnt: removes the attachment under the cursor.
    fn remove_attachment(&mut self) {
        let Some(compose) = self.compose.as_ref() else {
            return;
        };
        let Some(attachment) = compose.attachments.get(compose.attachment_cursor).cloned() else {
            self.status = Some(format!("No attachments: {ATTACH_KEY} attaches a file"));
            return;
        };
        let draft_id = compose.draft_id;
        match self
            .actions
            .remove_draft_attachment(draft_id, attachment.id, &mut |_| {})
        {
            Ok(()) => {
                if let Some(compose) = self.compose.as_mut() {
                    compose.attachments.retain(|kept| kept.id != attachment.id);
                    compose.attachment_cursor = compose
                        .attachment_cursor
                        .min(compose.attachments.len().saturating_sub(1));
                }
                self.status = Some(format!("Removed {}", attachment_label(&attachment)));
            }
            Err(error) => self.report(error),
        }
    }

    /// `^R`: inserts the text of the file at `path` at the body's cursor,
    /// filtered.
    pub(crate) fn read_into_body(&mut self, path: &Path) {
        let result = std::fs::metadata(path)
            .and_then(|metadata| {
                if metadata.len() > READ_FILE_LIMIT {
                    Err(std::io::Error::other(format!(
                        "it is larger than {}",
                        size_label(READ_FILE_LIMIT)
                    )))
                } else {
                    std::fs::read(path)
                }
            })
            .map(|bytes| sanitize(&String::from_utf8_lossy(&bytes)));
        let shown = sanitize_line(&path.display().to_string());
        match result {
            Ok(text) => {
                let Some(compose) = self.compose.as_mut() else {
                    return;
                };
                compose.focus = Field::Body;
                compose.body.insert_str(&text);
                compose.edited();
                if let Some(parent) = path.parent().filter(|parent| parent.is_dir()) {
                    self.browse_folder = Some(parent.to_path_buf());
                }
                self.status = Some(format!(
                    "Inserted {} from {shown}",
                    count_noun(text.lines().count(), "line", "lines")
                ));
            }
            Err(error) => {
                self.status = Some(format!(
                    "Couldn't read {shown}: {}",
                    sanitize_line(&error.to_string())
                ));
            }
        }
    }

    /// A key while a file prompt is open.
    pub(crate) fn path_key(&mut self, mut prompt: PathPrompt, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.prompt = None;
                return;
            }
            KeyCode::Char('c' | 'g') if control => {
                self.prompt = None;
                return;
            }
            KeyCode::Char('t') if control => {
                self.prompt = None;
                let typed = prompt.field.path();
                let folder = if typed.is_dir() {
                    Some(typed)
                } else {
                    typed.parent().map(Path::to_path_buf)
                };
                return self.open_file_browser(folder, prompt.purpose == PathPurpose::ReadFile);
            }
            KeyCode::Enter => {
                if prompt.field.input.trim().is_empty() {
                    self.prompt = None;
                    return;
                }
                let path = prompt.field.path();
                if path.is_dir() {
                    // Enter on a folder lists it, as Tab would.
                    prompt.field.complete(true);
                    self.prompt = Some(Prompt::Path(prompt));
                    return;
                }
                self.prompt = None;
                return match prompt.purpose {
                    PathPurpose::Attach => self.attach(&path),
                    PathPurpose::ReadFile => self.read_into_body(&path),
                };
            }
            KeyCode::Tab => prompt.field.complete(true),
            KeyCode::BackTab => prompt.field.complete(false),
            KeyCode::Backspace => {
                prompt.field.input.pop();
                prompt.field.edited();
            }
            KeyCode::Char('u') if control => {
                prompt.field.input.clear();
                prompt.field.edited();
            }
            KeyCode::Char(ch) if !control && !ch.is_control() => {
                prompt.field.input.push(ch);
                prompt.field.edited();
            }
            _ => return,
        }
        self.prompt = Some(Prompt::Path(prompt));
    }

    // ------------------------------------------------------------------
    // Saving, sending, postponing and cancelling
    // ------------------------------------------------------------------

    /// Writes the open draft's fields to the store.
    pub(crate) fn save_compose(&mut self) -> anyhow::Result<()> {
        let Some(compose) = self.compose.as_mut() else {
            return Ok(());
        };
        let update = compose.update();
        compose.dirty = false;
        let draft_id = compose.draft_id;
        self.actions.save_draft(draft_id, &update, &mut |_| {})
    }

    /// Saves the draft once typing has paused for [`AUTOSAVE_DELAY`], as
    /// the GUI does. Called every tick.
    pub(crate) fn autosave_if_due(&mut self) {
        let due = self
            .compose
            .as_ref()
            .is_some_and(|compose| compose.dirty && compose.last_edit.elapsed() >= AUTOSAVE_DELAY);
        if due && let Err(error) = self.save_compose() {
            self.report(error);
        }
    }

    /// Leaves the Compose screen for where it was opened from.
    fn close_compose(&mut self) {
        self.compose = None;
        let back = match self.compose_from {
            Screen::Compose | Screen::Help => Screen::MainMenu,
            screen => screen,
        };
        self.goto(back);
        self.refresh();
    }

    /// `^O`: saves the draft and leaves it for later.
    fn postpone(&mut self) {
        if let Err(error) = self.save_compose() {
            return self.report(error);
        }
        self.close_compose();
        self.status = Some("Message postponed. C continues it".to_owned());
    }

    /// `^C`: a draft that hasn't changed is left (a postponed one) or
    /// deleted (a new one) without asking; otherwise Zmail asks first.
    fn cancel_compose(&mut self) {
        let Some(compose) = self.compose.as_ref() else {
            return;
        };
        if compose.changed() {
            self.prompt = Some(Prompt::Confirm(Confirm::Cancel));
        } else if compose.created {
            self.abandon();
        } else {
            self.close_compose();
            self.status = Some("The postponed message is unchanged".to_owned());
        }
    }

    /// Deletes the open draft and leaves.
    pub(crate) fn abandon(&mut self) {
        let Some(draft_id) = self.compose.as_ref().map(|compose| compose.draft_id) else {
            return;
        };
        let mut events = Vec::new();
        let result = self.actions.delete_draft(draft_id, &mut log(&mut events));
        self.close_compose();
        match result {
            Ok(()) => self.status = Some("Message cancelled".to_owned()),
            Err(error) => self.report(error),
        }
    }

    /// `^X`, confirmed: checks the recipients, saves, and queues the draft
    /// through the sync service as the GUI does, after the undo-send
    /// delay. Another Zmail that holds the sync lock sends it from the
    /// outbox; with no sync service (the fixture) it only queues.
    pub(crate) fn send(&mut self) {
        let Some(compose) = self.compose.as_mut() else {
            return;
        };
        let update = compose.update();
        if update.to.is_empty() && update.cc.is_empty() && update.bcc.is_empty() {
            compose.focus = Field::To;
            self.status = Some("Add a recipient in To, Cc or Bcc first".to_owned());
            return;
        }
        let invalid: Vec<String> = [Field::To, Field::Cc, Field::Bcc]
            .into_iter()
            .flat_map(|field| invalid_addresses(&compose.text(field)))
            .collect();
        if !invalid.is_empty() {
            self.status = Some(format!(
                "Not an address Zmail can send to: {}",
                invalid.join(", ")
            ));
            return;
        }
        let draft_id = compose.draft_id;
        if let Err(error) = self.save_compose() {
            return self.report(error);
        }
        let delay = self
            .store
            .ui_preferences()
            .map(|preferences| preferences.undo_send_delay())
            .unwrap_or_default();
        let queued = match &self.sync {
            Some(sync) => sync.queue_draft_and_send_after(draft_id, delay),
            None => self.store.queue_draft_after(draft_id, delay),
        };
        let job_id = match queued {
            Ok(job_id) => job_id,
            Err(error) => return self.report(error),
        };
        self.close_compose();
        let sender = match &self.sync {
            Some(sync) if sync.runs_sync() => "",
            Some(_) => " (the Zmail that syncs sends it)",
            None => " (nothing sends mail from here)",
        };
        self.status = Some(if delay.is_zero() {
            format!("Message queued to send{sender}")
        } else {
            self.pending_send = Some(PendingSend {
                job_id,
                draft_id,
                until: Instant::now() + delay,
            });
            format!(
                "Sending in {}{sender}. U Undo",
                count_noun(delay.as_secs() as usize, "second", "seconds")
            )
        });
    }

    /// Whether `U` can still take back the message just sent.
    pub(crate) fn can_undo_send(&self) -> bool {
        self.pending_send
            .is_some_and(|pending| Instant::now() < pending.until)
    }

    /// `U` during the undo-send delay: takes the message back and opens
    /// it again, as the GUI's Undo does.
    pub(crate) fn undo_send(&mut self) {
        let Some(pending) = self.pending_send.take() else {
            return;
        };
        match self
            .actions
            .cancel_send(pending.job_id, pending.draft_id, &mut |_| {})
        {
            Ok(true) => {
                self.open_compose(pending.draft_id, false, Field::Body);
                self.status = Some("Sending undone".to_owned());
            }
            Ok(false) => {
                self.status = Some("Too late to undo: the message has started sending".to_owned());
            }
            Err(error) => self.report(error),
        }
    }

    /// Forgets the undo-send offer once its delay has passed.
    pub(crate) fn expire_pending_send(&mut self) {
        if self
            .pending_send
            .is_some_and(|pending| Instant::now() >= pending.until)
        {
            self.pending_send = None;
        }
    }
}

impl App {
    /// The command bar on the Compose screen: Pico's keys for the field
    /// with the cursor, or the `^T` list's.
    pub(crate) fn compose_commands(&self) -> Vec<crate::app::CommandEntry> {
        use crate::app::{Cmd, entry};
        let Some(compose) = self.compose.as_ref() else {
            return Vec::new();
        };
        let pico = |key, label, pico| entry(key, label, Cmd::Pico(pico));
        if let Some(picker) = &compose.picker {
            let mut entries = vec![
                entry("Ret", "[Select]", Cmd::Select),
                entry("P", "Prev", Cmd::Prev),
                entry("N", "Next", Cmd::Next),
            ];
            if matches!(picker.kind, PickerKind::Files { .. }) {
                entries.push(entry("<", "Parent", Cmd::Back));
            }
            entries.push(entry("Esc", "Back", Cmd::Back));
            return entries;
        }
        // In pairs, top row then bottom row, as Pico lays them out.
        let mut entries = vec![
            pico("^G", "Get Help", Pico::Help),
            pico("^C", "Cancel", Pico::Cancel),
            pico("^X", "Send", Pico::Send),
            pico(ATTACH_KEY, "Attach", Pico::Attach),
        ];
        match compose.focus {
            Field::Body => {
                entries.extend([
                    pico("^R", "Read File", Pico::ReadFile),
                    pico("^W", "Where is", Pico::WhereIs),
                    pico("^K", "Cut Line", Pico::Cut),
                    pico("^U", "UnCut", Pico::Uncut),
                    pico("^O", "Postpone", Pico::Postpone),
                ]);
                if self.prefs.external_editor {
                    entries.push(pico("^_", "Alt Edit", Pico::Editor));
                }
            }
            Field::Attachments => entries.extend([
                pico("^R", "Read File", Pico::ReadFile),
                pico("^T", "Browse", Pico::Browse),
                pico("^K", "Remove", Pico::Cut),
                pico("^O", "Postpone", Pico::Postpone),
            ]),
            Field::From => entries.extend([
                pico("Spc", "Next From", Pico::NextFrom),
                pico("^O", "Postpone", Pico::Postpone),
            ]),
            Field::Subject => entries.extend([
                pico("^R", "Read File", Pico::ReadFile),
                pico("^K", "Cut Text", Pico::Cut),
                pico("^U", "UnCut", Pico::Uncut),
                pico("^O", "Postpone", Pico::Postpone),
            ]),
            Field::To | Field::Cc | Field::Bcc => entries.extend([
                pico("^R", "Read File", Pico::ReadFile),
                pico("Tab", "Complete", Pico::Complete),
                pico("^K", "Cut Text", Pico::Cut),
                pico("^U", "UnCut", Pico::Uncut),
                pico("^O", "Postpone", Pico::Postpone),
                pico("^T", "To AddrBk", Pico::Browse),
            ]),
        }
        entries
    }

    /// The mouse on the Compose screen: the command bar, the `^T` list,
    /// a click on a field to move the cursor there, and the wheel.
    pub(crate) fn compose_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let (x, y) = (mouse.column, mouse.row);
        let position = ratatui::layout::Position { x, y };
        let picker_open = self
            .compose
            .as_ref()
            .is_some_and(|compose| compose.picker.is_some());
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(cmd) = self
                    .command_targets
                    .iter()
                    .find(|(area, _)| area.contains(position))
                    .map(|(_, cmd)| *cmd)
                {
                    self.status = None;
                    return match cmd {
                        crate::app::Cmd::Pico(pico) => self.compose_key(pico.key()),
                        crate::app::Cmd::Select => self.pick(),
                        crate::app::Cmd::Prev => self.compose_key(KeyEvent::from(KeyCode::Up)),
                        crate::app::Cmd::Next => self.compose_key(KeyEvent::from(KeyCode::Down)),
                        crate::app::Cmd::Back if picker_open => {
                            self.compose_key(KeyEvent::from(KeyCode::Esc))
                        }
                        _ => {}
                    };
                }
                if picker_open {
                    let target = self
                        .panes
                        .iter()
                        .find(|pane| pane.id == crate::pane::PaneId::Picker && pane.contains(x, y))
                        .and_then(|pane| pane.lines.get(pane.point_at(x, y).line))
                        .and_then(|line| line.target);
                    if let Some(target) = target
                        && let Some(picker) = self
                            .compose
                            .as_mut()
                            .and_then(|compose| compose.picker.as_mut())
                    {
                        picker.cursor = target;
                        self.pick();
                    }
                    return;
                }
                let target = self
                    .compose_targets
                    .iter()
                    .find(|(area, _, _)| area.contains(position))
                    .map(|(_, field, attachment)| (*field, *attachment));
                if let Some((field, attachment)) = target
                    && let Some(compose) = self.compose.as_mut()
                {
                    compose.focus = field;
                    compose.completion = None;
                    if let Some(attachment) = attachment {
                        compose.attachment_cursor = attachment;
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let code = if mouse.kind == MouseEventKind::ScrollUp {
                    KeyCode::Up
                } else {
                    KeyCode::Down
                };
                if picker_open {
                    self.compose_key(KeyEvent::from(code));
                } else if let Some(compose) = self.compose.as_mut()
                    && compose.focus == Field::Body
                {
                    let movement = if code == KeyCode::Up {
                        CursorMove::Up
                    } else {
                        CursorMove::Down
                    };
                    for _ in 0..3 {
                        compose.body.move_cursor(movement);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The section of Help that `^G` opens on.
pub const COMPOSE_HELP_TOPIC: &str = "Writing mail";

/// `Name <address>` from the address history, filtered: names come from
/// mail.
fn filtered_address(name: Option<&str>, email: &str) -> String {
    Participant {
        name: name.map(sanitize_line).filter(|name| !name.is_empty()),
        email: Some(sanitize_line(email)),
    }
    .to_header_value()
}

#[cfg(test)]
mod tests {
    use super::{invalid_addresses, last_address_start, recipients};

    #[test]
    fn address_fields_save_as_the_guis_recipient_chips() {
        assert_eq!(
            recipients(
                "Ada <ada@example.com>, \"Lee, Carol\" <carol@example.com>; bo@example.com,"
            ),
            [
                "Ada <ada@example.com>",
                "\"Lee, Carol\" <carol@example.com>",
                "bo@example.com"
            ]
        );
        assert!(recipients(" , ").is_empty());
        assert_eq!(
            invalid_addresses("ada@example.com, bob, carol@, d@x..y"),
            ["bob", "carol@", "d@x..y"]
        );
    }

    #[test]
    fn the_address_being_typed_starts_after_the_last_separator() {
        assert_eq!(last_address_start("ad"), 0);
        assert_eq!(last_address_start("ada@example.com,  gr"), 18);
        assert_eq!(last_address_start("\"Lee, Carol\" <c@x>; b"), 20);
    }
}
