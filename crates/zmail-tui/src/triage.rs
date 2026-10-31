//! Triage: delete, archive, save (move) to a folder, flag, and mark read
//! or unread, on the message under the cursor or, with Alpine's Apply, on
//! the messages selected with `;`; and Undo.
//!
//! Every change goes through [`zmail_actions::MailActions`], the GUI's own
//! service: a store write with its server write-back queued, so it works
//! whichever Zmail holds the sync lock (the holder sends it at its next
//! sync), and one undo stack for the session, so Undo reaches a change
//! made on any screen. A conversation is the unit, as in the GUI: in an
//! index that lists single messages, a command acts on the message's
//! whole conversation, and the status line says so.

use std::{collections::BTreeSet, time::Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use zmail_actions::{MailActions, MailEvent};
use zmail_core::{MailboxSelection, text::count_noun};
use zmail_security::terminal::sanitize_line;

use crate::{
    app::{App, Prompt, Screen},
    pane::PaneId,
};

/// A triage command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Triage {
    /// Delete to the Trash.
    Delete,
    /// Archive.
    Archive,
    /// Save (move) to a folder, after asking which.
    Save,
    /// Flag, or unflag when everything is flagged.
    Flag,
    /// Mark read, or unread when everything is read.
    ReadState,
}

/// A folder the save prompt offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveFolder {
    /// The mailbox.
    pub mailbox_id: i64,
    /// Its full name, filtered for the terminal.
    pub name: String,
}

/// Alpine's "Save to folder" field: Tab completes over the account's
/// folders, and `^T` lists them to pick from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavePrompt {
    /// The conversations to move.
    pub thread_ids: Vec<i64>,
    /// The account's folders, except the one shown.
    pub folders: Vec<SaveFolder>,
    /// What has been typed, pasted or completed.
    pub input: String,
    /// What the last Tab (or an Enter that matched nothing) found, as
    /// indexes into `folders`; shown above the status line until the field
    /// changes.
    pub matches: Option<Vec<usize>>,
    /// Which of `matches` Tab filled in, once Tab cycles through them.
    pub cycle: Option<usize>,
    /// The folder list's cursor, while `^T` shows it.
    pub picker: Option<usize>,
}

impl SavePrompt {
    /// The folders whose name starts with what was typed, ignoring case,
    /// or failing that the ones that contain it.
    pub fn candidates(&self) -> Vec<usize> {
        let needle = self.input.to_lowercase();
        let matching = |test: &dyn Fn(&str) -> bool| -> Vec<usize> {
            self.folders
                .iter()
                .enumerate()
                .filter(|(_, folder)| test(&folder.name.to_lowercase()))
                .map(|(index, _)| index)
                .collect()
        };
        let starting = matching(&|name| name.starts_with(&needle));
        if starting.is_empty() {
            matching(&|name| name.contains(&needle))
        } else {
            starting
        }
    }

    /// Tab (or Shift-Tab, `forward` false): completes as far as the
    /// matching folders agree, then cycles through them.
    pub fn complete(&mut self, forward: bool) {
        if let (Some(matches), Some(cycle)) = (&self.matches, self.cycle)
            && matches.len() > 1
        {
            let count = matches.len();
            let next = if forward {
                (cycle + 1) % count
            } else {
                (cycle + count - 1) % count
            };
            self.cycle = Some(next);
            self.input = self.folders[matches[next]].name.clone();
            return;
        }
        let matches = self.candidates();
        match matches.as_slice() {
            [] => {}
            [only] => {
                self.input = self.folders[*only].name.clone();
            }
            _ => {
                let prefix = common_prefix(matches.iter().map(|&index| &self.folders[index].name));
                let typed = self.input.to_lowercase();
                if prefix.chars().count() > self.input.chars().count()
                    && prefix.to_lowercase().starts_with(&typed)
                {
                    self.input = prefix;
                } else {
                    let first = if forward { 0 } else { matches.len() - 1 };
                    self.cycle = Some(first);
                    self.input = self.folders[matches[first]].name.clone();
                }
            }
        }
        self.matches = Some(matches);
    }

    /// The folder Enter saves to: the picked one, the one named exactly
    /// (ignoring case), or the only one that matches.
    pub fn chosen(&self) -> Option<&SaveFolder> {
        if let Some(cursor) = self.picker {
            return self.folders.get(cursor);
        }
        let typed = self.input.trim().to_lowercase();
        if typed.is_empty() {
            return None;
        }
        self.folders
            .iter()
            .find(|folder| folder.name.to_lowercase() == typed)
            .or_else(|| match self.candidates().as_slice() {
                [only] => self.folders.get(*only),
                _ => None,
            })
    }

    /// The field changed: forget what Tab found.
    pub fn edited(&mut self) {
        self.matches = None;
        self.cycle = None;
    }

    /// The question on the status line.
    pub fn label(&self) -> String {
        if self.picker.is_some() {
            return "Save to which folder? Enter saves, Esc goes back to typing".to_owned();
        }
        let what = if self.thread_ids.len() == 1 {
            String::new()
        } else {
            format!(
                " {}",
                count_noun(self.thread_ids.len(), "conversation", "conversations")
            )
        };
        format!("Save{what} to folder: {}", self.input)
    }
}

/// The longest start the names share, ignoring case, as the first one
/// spells it.
fn common_prefix<'a>(mut names: impl Iterator<Item = &'a String>) -> String {
    let Some(first) = names.next() else {
        return String::new();
    };
    let mut length = first.chars().count();
    for name in names {
        length = first
            .chars()
            .zip(name.chars())
            .take(length)
            .take_while(|(left, right)| left.to_lowercase().eq(right.to_lowercase()))
            .count();
    }
    first.chars().take(length).collect()
}

/// A callback that logs a change's events.
fn log(events: &mut Vec<MailEvent>) -> impl FnMut(MailEvent) + '_ {
    move |event| events.push(event)
}

impl App {
    /// The conversation a command acts on: the one open in Message Text,
    /// or the one under the index's cursor.
    fn current_thread(&self) -> Option<i64> {
        match self.screen {
            Screen::Message => self
                .message
                .as_ref()
                .map(|state| state.conversation.thread_id),
            Screen::Index => self
                .index
                .entries
                .get(self.index.cursor)
                .map(|entry| entry.thread_id),
            _ => None,
        }
    }

    fn listed(&self, thread_id: i64) -> bool {
        self.index
            .entries
            .iter()
            .any(|entry| entry.thread_id == thread_id)
    }

    /// `command` on the current conversation.
    pub(crate) fn triage(&mut self, command: Triage) {
        match self.current_thread() {
            Some(thread_id) => self.triage_threads(command, vec![thread_id]),
            None => self.status = Some("No messages here".to_owned()),
        }
    }

    /// `command` on `thread_ids`.
    pub(crate) fn triage_threads(&mut self, command: Triage, thread_ids: Vec<i64>) {
        if thread_ids.is_empty() {
            return;
        }
        let states: Vec<_> = thread_ids
            .iter()
            .filter_map(|&thread_id| self.store.thread_state(thread_id).ok().flatten())
            .collect();
        match command {
            Triage::Delete => self.change(&thread_ids, |actions, ids, events| {
                actions.delete_threads(ids, &mut log(events))
            }),
            Triage::Archive => self.change(&thread_ids, |actions, ids, events| {
                actions.archive_threads(ids, &mut log(events))
            }),
            Triage::Flag => {
                let flagged = !states.iter().all(|thread| thread.is_flagged());
                self.change(&thread_ids, |actions, ids, events| {
                    actions.set_threads_flagged(ids, flagged, &mut log(events))
                });
            }
            Triage::ReadState => {
                let read = !states.iter().all(|thread| thread.is_read());
                // The user's choice wins over marking an open message read.
                if self
                    .mark_read
                    .is_some_and(|(thread_id, _)| thread_ids.contains(&thread_id))
                {
                    self.mark_read = None;
                }
                self.change(&thread_ids, |actions, ids, events| {
                    actions.set_threads_read(ids, read, &mut log(events))
                });
            }
            Triage::Save => self.open_save(thread_ids, &states),
        }
    }

    /// Runs a change, reads the store again, and says what it did with
    /// its undo hint, as the GUI's toast does.
    fn change(
        &mut self,
        thread_ids: &[i64],
        run: impl FnOnce(&mut MailActions, &[i64], &mut Vec<MailEvent>) -> anyhow::Result<()>,
    ) {
        let row = self.index.cursor;
        let under_cursor = self.index.entries.get(row).map(|entry| entry.thread_id);
        let open = self
            .message
            .as_ref()
            .map(|state| state.conversation.thread_id)
            .filter(|thread_id| self.screen == Screen::Message && self.listed(*thread_id));
        let mut events = Vec::new();
        let result = run(&mut self.actions, thread_ids, &mut events);
        self.reread(row, under_cursor);
        // Message Text moves on to the next message when the open one left
        // the folder, as Alpine does after a delete.
        if let Some(open) = open
            && !self.listed(open)
        {
            if self.index.entries.is_empty() {
                self.message = None;
                self.screen = Screen::Index;
            } else {
                self.open_entry(self.index.cursor);
            }
        }
        match result {
            Ok(()) => {
                let recorded = events.iter().find_map(|event| match event {
                    MailEvent::UndoRecorded {
                        label, can_undo, ..
                    } => Some((label.clone(), *can_undo)),
                    _ => None,
                });
                // The label may name a folder, from the server.
                self.status = Some(match recorded {
                    Some((label, true)) => format!("{}. U Undo", sanitize_line(&label)),
                    Some((label, false)) => sanitize_line(&label),
                    None => "Nothing changed".to_owned(),
                });
            }
            Err(error) => self.report(error),
        }
    }

    /// Reads the store again after a change, keeping the cursor on its row
    /// when the conversation it was on has gone, and drops the selected
    /// conversations that left the folder.
    fn reread(&mut self, row: usize, under_cursor: Option<i64>) {
        self.refresh();
        if under_cursor.is_some_and(|thread_id| !self.listed(thread_id)) {
            self.index.cursor = row.min(self.index.entries.len().saturating_sub(1));
            self.reveal.insert(PaneId::Index);
        }
        if let Some(folder) = &self.folder {
            let store = &self.store;
            self.selected.retain(|thread_id| {
                store
                    .selection_contains(&folder.selection, *thread_id)
                    .unwrap_or(false)
            });
        }
    }

    /// U or z: reverses the last change, made on whichever screen.
    pub(crate) fn undo(&mut self) {
        let row = self.index.cursor;
        let under_cursor = self.index.entries.get(row).map(|entry| entry.thread_id);
        let result = self.actions.undo(&mut |_| {});
        self.reread(row, under_cursor);
        match result {
            Ok(Some(entry)) => {
                // In the index, the cursor goes to what came back.
                if self.screen == Screen::Index
                    && let Some(position) = entry.thread_ids().find_map(|thread_id| {
                        self.index
                            .entries
                            .iter()
                            .position(|listed| listed.thread_id == thread_id)
                    })
                {
                    self.index.cursor = position;
                    self.reveal.insert(PaneId::Index);
                }
                self.status = Some(format!("Undone: {}", sanitize_line(&entry.label)));
            }
            Ok(None) => self.status = Some("Nothing to undo".to_owned()),
            Err(error) => self.report(error),
        }
    }

    /// `;` or a click on the marker column: selects the conversation on
    /// `row`, or unselects it.
    pub(crate) fn toggle_selected(&mut self, row: usize) {
        let Some(thread_id) = self.index.entries.get(row).map(|entry| entry.thread_id) else {
            return;
        };
        if !self.selected.remove(&thread_id) {
            self.selected.insert(thread_id);
        }
        self.status = Some(if self.selected.is_empty() {
            "Nothing selected".to_owned()
        } else {
            format!(
                "{} selected. A applies a command to them",
                count_noun(self.selected.len(), "conversation", "conversations")
            )
        });
    }

    /// `;` in the Apply question: unselects everything.
    pub(crate) fn unselect_all(&mut self) {
        let count = self.selected.len();
        self.selected.clear();
        self.status = Some(format!(
            "Unselected {}",
            count_noun(count, "conversation", "conversations")
        ));
    }

    /// A: asks which command to apply to the selection.
    pub(crate) fn apply(&mut self) {
        if self.selected.is_empty() {
            self.status = Some("Select messages with ; first, then A applies a command".to_owned());
            return;
        }
        self.prompt = Some(Prompt::Apply);
    }

    /// Asks which folder of their account to save `thread_ids` to.
    fn open_save(&mut self, thread_ids: Vec<i64>, states: &[zmail_core::ThreadState]) {
        let accounts: BTreeSet<i64> = states.iter().map(|thread| thread.account_id).collect();
        let account_id = match accounts.iter().collect::<Vec<_>>().as_slice() {
            [] => {
                self.status = Some("That conversation is gone".to_owned());
                return;
            }
            [account_id] => **account_id,
            _ => {
                self.status = Some(
                    "The selected messages are in different accounts: save them one account at \
                     a time"
                        .to_owned(),
                );
                return;
            }
        };
        let shown = match self.folder.as_ref().map(|folder| folder.selection) {
            Some(MailboxSelection::Mailbox { mailbox_id, .. }) => Some(mailbox_id),
            _ => None,
        };
        let folders: Vec<SaveFolder> = match self.store.list_mailboxes(account_id) {
            Ok(mailboxes) => mailboxes
                .into_iter()
                .filter(|mailbox| Some(mailbox.id) != shown)
                .map(|mailbox| SaveFolder {
                    mailbox_id: mailbox.id,
                    name: sanitize_line(&mailbox.name),
                })
                .collect(),
            Err(error) => return self.report(error),
        };
        if folders.is_empty() {
            self.status = Some("There's no other folder to save to".to_owned());
            return;
        }
        self.prompt = Some(Prompt::Save(SavePrompt {
            thread_ids,
            folders,
            input: String::new(),
            matches: None,
            cycle: None,
            picker: None,
        }));
    }

    /// A key while the save prompt is open.
    pub(crate) fn save_key(&mut self, mut save: SavePrompt, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(cursor) = save.picker {
            let last = save.folders.len().saturating_sub(1);
            let page = self
                .pane(PaneId::Picker)
                .map_or(10, |pane| pane.height().saturating_sub(3).max(1));
            save.picker = Some(match key.code {
                KeyCode::Esc => {
                    save.picker = None;
                    self.prompt = Some(Prompt::Save(save));
                    return;
                }
                KeyCode::Char('c' | 'g') if control => {
                    self.prompt = None;
                    return;
                }
                KeyCode::Enter => return self.save_to_chosen(save),
                KeyCode::Up | KeyCode::Char('p' | 'P') => cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('n' | 'N') => (cursor + 1).min(last),
                KeyCode::PageUp | KeyCode::Char('-') => cursor.saturating_sub(page),
                KeyCode::PageDown | KeyCode::Char(' ') => (cursor + page).min(last),
                KeyCode::Home => 0,
                KeyCode::End => last,
                _ => return,
            });
            self.reveal.insert(PaneId::Picker);
            self.prompt = Some(Prompt::Save(save));
            return;
        }
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
                let cursor = save
                    .chosen()
                    .and_then(|chosen| save.folders.iter().position(|folder| folder == chosen))
                    .or_else(|| save.candidates().first().copied())
                    .unwrap_or(0);
                save.picker = Some(cursor);
                self.reveal.insert(PaneId::Picker);
            }
            KeyCode::Enter => return self.save_to_chosen(save),
            KeyCode::Tab => save.complete(true),
            KeyCode::BackTab => save.complete(false),
            KeyCode::Backspace => {
                save.input.pop();
                save.edited();
            }
            KeyCode::Char('u') if control => {
                save.input.clear();
                save.edited();
            }
            KeyCode::Char(ch) if !control && !ch.is_control() => {
                save.input.push(ch);
                save.edited();
            }
            _ => return,
        }
        self.prompt = Some(Prompt::Save(save));
    }

    /// Enter in the save prompt: moves to the folder chosen, or shows
    /// what matches when the name doesn't name one.
    fn save_to_chosen(&mut self, mut save: SavePrompt) {
        let Some(folder) = save.chosen().cloned() else {
            if save.input.trim().is_empty() {
                self.prompt = None;
            } else {
                save.matches = Some(save.candidates());
                save.cycle = None;
                self.prompt = Some(Prompt::Save(save));
            }
            return;
        };
        self.prompt = None;
        self.change(&save.thread_ids, |actions, ids, events| {
            actions.move_threads(ids, folder.mailbox_id, &mut log(events))
        });
    }

    /// The mouse on the save prompt's folder list: a click saves to the
    /// folder, the wheel moves. `true` when it was used.
    pub(crate) fn picker_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(Prompt::Save(save)) = &self.prompt else {
            return false;
        };
        let Some(cursor) = save.picker else {
            return false;
        };
        let Some(pane) = self
            .pane(PaneId::Picker)
            .filter(|pane| pane.contains(mouse.column, mouse.row))
        else {
            return false;
        };
        let target = pane
            .lines
            .get(pane.point_at(mouse.column, mouse.row).line)
            .and_then(|line| line.target);
        let last = save.folders.len().saturating_sub(1);
        let mut save = save.clone();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(target) = target {
                    save.picker = Some(target);
                    self.save_to_chosen(save);
                }
                return true;
            }
            MouseEventKind::ScrollUp => save.picker = Some(cursor.saturating_sub(1)),
            MouseEventKind::ScrollDown => save.picker = Some((cursor + 1).min(last)),
            _ => return true,
        }
        self.reveal.insert(PaneId::Picker);
        self.prompt = Some(Prompt::Save(save));
        true
    }

    /// Marks the conversation open in Message Text read once it has stayed
    /// open for [`zmail_actions::MARK_READ_DELAY`], as the GUI's reader
    /// does; not when the user has moved on, or marked it unread.
    pub(crate) fn mark_read_if_due(&mut self) {
        let Some((thread_id, due)) = self.mark_read else {
            return;
        };
        if Instant::now() < due {
            return;
        }
        self.mark_read = None;
        let still_open = self.screen == Screen::Message
            && self
                .message
                .as_ref()
                .is_some_and(|state| state.conversation.thread_id == thread_id);
        let unread = self
            .store
            .thread_state(thread_id)
            .ok()
            .flatten()
            .is_some_and(|thread| !thread.is_read());
        if !still_open || !unread {
            return;
        }
        match self.actions.mark_opened_thread_read(thread_id, &mut |_| {}) {
            Ok(()) => self.refresh(),
            Err(error) => self.report(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SaveFolder, SavePrompt};

    fn prompt(names: &[&str], input: &str) -> SavePrompt {
        SavePrompt {
            thread_ids: vec![1],
            folders: names
                .iter()
                .enumerate()
                .map(|(index, name)| SaveFolder {
                    mailbox_id: index as i64,
                    name: (*name).to_owned(),
                })
                .collect(),
            input: input.to_owned(),
            matches: None,
            cycle: None,
            picker: None,
        }
    }

    #[test]
    fn tab_completes_as_far_as_the_folders_agree_then_cycles() {
        let mut save = prompt(
            &["Archive", "Projects", "Projects/Website", "Receipts"],
            "p",
        );
        save.complete(true);
        assert_eq!(save.input, "Projects");
        assert_eq!(save.matches, Some(vec![1, 2]));
        save.complete(true);
        assert_eq!(save.input, "Projects", "the first match");
        save.complete(true);
        assert_eq!(save.input, "Projects/Website");
        save.complete(true);
        assert_eq!(save.input, "Projects");
        save.complete(false);
        assert_eq!(save.input, "Projects/Website");
        // Typing starts again.
        save.input = "re".to_owned();
        save.edited();
        save.complete(true);
        assert_eq!(save.input, "Receipts");
        assert_eq!(save.chosen().map(|folder| folder.mailbox_id), Some(3));
    }

    #[test]
    fn completion_ignores_case_and_falls_back_to_contains() {
        let mut save = prompt(&["INBOX", "Archive", "Old/Receipts"], "inb");
        save.complete(true);
        assert_eq!(save.input, "INBOX");
        let mut save = prompt(&["INBOX", "Archive", "Old/Receipts"], "ceipt");
        save.complete(true);
        assert_eq!(save.input, "Old/Receipts");
        let mut save = prompt(&["INBOX", "Archive"], "zzz");
        save.complete(true);
        assert_eq!(save.input, "zzz");
        assert_eq!(save.matches, Some(Vec::new()));
        assert_eq!(save.chosen(), None);
    }

    #[test]
    fn enter_takes_the_exact_name_or_the_only_match() {
        let save = prompt(&["Projects", "Projects/Website"], "projects");
        assert_eq!(save.chosen().map(|folder| folder.mailbox_id), Some(0));
        let save = prompt(&["Projects", "Projects/Website"], "proj");
        assert_eq!(save.chosen(), None, "two match");
        let save = prompt(&["Projects", "Projects/Website"], "web");
        assert_eq!(save.chosen().map(|folder| folder.mailbox_id), Some(1));
    }
}
