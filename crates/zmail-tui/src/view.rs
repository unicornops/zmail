//! Drawing: Alpine's three bands (the title bar, the screen's body, the
//! status line and two-line command bar) on every screen.
//!
//! Every string drawn from mail was filtered when it was read (`data.rs`,
//! `message.rs`); the rest is Zmail's own text.

use std::ops::Range;

use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use zmail_core::{preferences::CopyClipboard, time::format_list_date};
use zmail_security::sender_safety::SafetyTier;

mod accounts;
mod compose;
mod identities;

use crate::{
    app::{App, MENU_ITEMS, Prompt, SETUP_ROWS, Screen, SetupRow},
    clipboard::{CopyTarget, copy_target},
    data::{FolderRow, LocalFolder},
    message::{Conversation, DocLine},
    palette::Palette,
    pane::{DrawnPane, PaneId, PaneLine},
    text::{self, column_of, fit, fit_right, wrap},
    triage::SavePrompt,
};

/// The Message Index's first column, which marks the conversations
/// selected with `;` (Alpine's `X`); a click on it selects.
pub(crate) const MARKER_WIDTH: usize = 2;

/// Styles and characters for this terminal: colours unless `NO_COLOR`,
/// box drawing unless ASCII.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Theme {
    color: bool,
    unicode: bool,
}

impl Theme {
    pub(crate) fn of(app: &App) -> Self {
        Self {
            color: app.env.color,
            unicode: app.env.unicode && !app.prefs.ascii,
        }
    }

    fn reversed(self) -> Style {
        Style::new().add_modifier(Modifier::REVERSED)
    }

    fn bold(self) -> Style {
        Style::new().add_modifier(Modifier::BOLD)
    }

    fn flag(self) -> Style {
        if self.color {
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            self.bold()
        }
    }

    fn link(self) -> Style {
        let style = Style::new().add_modifier(Modifier::UNDERLINED);
        if self.color {
            style.fg(Color::Cyan)
        } else {
            style
        }
    }

    fn faint(self) -> Style {
        if self.color {
            Style::new().fg(Color::DarkGray)
        } else {
            Style::new()
        }
    }

    /// A sender-safety warning: red for a likely impersonation, yellow for
    /// replies going elsewhere, cyan for a related address; bold without
    /// colour, except the mildest.
    fn warning(self, tier: SafetyTier) -> Style {
        match (tier, self.color) {
            (SafetyTier::Danger, true) => Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            (SafetyTier::Caution, true) => {
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            }
            (SafetyTier::Info, true) => Style::new().fg(Color::Cyan),
            (SafetyTier::Info, false) => Style::new(),
            (_, false) => self.bold(),
        }
    }

    fn found(self) -> Style {
        if self.color {
            Style::new().bg(Color::Yellow).fg(Color::Black)
        } else {
            self.reversed().add_modifier(Modifier::BOLD)
        }
    }

    fn vertical(self) -> &'static str {
        if self.unicode { "│" } else { "|" }
    }

    fn horizontal(self) -> &'static str {
        if self.unicode { "─" } else { "-" }
    }

    fn ellipsis(self) -> &'static str {
        if self.unicode { "…" } else { "..." }
    }
}

/// Draws the whole screen and records what was drawn where.
pub fn draw(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    app.size = (area.width, area.height);
    app.panes.clear();
    app.command_targets.clear();
    let theme = Theme::of(app);
    if area.height < 6 || area.width < 20 {
        frame.buffer_mut().set_stringn(
            0,
            0,
            "Zmail needs a bigger terminal",
            usize::from(area.width),
            Style::new(),
        );
        return;
    }
    let width = area.width;
    let body = Rect::new(0, 1, width, area.height - 4);
    draw_title(app, frame, theme, Rect::new(0, 0, width, 1));
    match app.screen {
        Screen::MainMenu => {
            let lines = menu_lines(app, theme, width);
            let cursor = app.menu_cursor + 2;
            draw_pane(app, frame, PaneId::Menu, body, lines, Some(cursor));
        }
        Screen::FolderList => {
            let lines = folder_lines(app, theme, width);
            let cursor = app.folder_cursor;
            draw_pane(app, frame, PaneId::Folders, body, lines, Some(cursor));
        }
        Screen::Index => {
            if app.preview_shown(width) {
                let left = (width - 1) / 2;
                let index_area = Rect::new(0, body.y, left, body.height);
                // One column for the rule and one of space before the text.
                let preview_area = Rect::new(left + 2, body.y, width - left - 2, body.height);
                draw_index(app, frame, theme, index_area);
                for y in body.y..body.y + body.height {
                    frame
                        .buffer_mut()
                        .set_string(left, y, theme.vertical(), theme.faint());
                }
                // "Looks safe" works in Message Text, not on the preview.
                let lines = match app.preview_conversation() {
                    Some(conversation) => {
                        message_lines(conversation, preview_area.width, theme, None, None, None).0
                    }
                    None => Vec::new(),
                };
                draw_pane(app, frame, PaneId::Preview, preview_area, lines, None);
            } else {
                draw_index(app, frame, theme, body);
            }
        }
        Screen::Message => draw_message(app, frame, theme, body),
        Screen::Setup => {
            let lines = setup_lines(app, theme, width);
            let cursor = app.setup_cursor + 3;
            draw_pane(app, frame, PaneId::Setup, body, lines, Some(cursor));
        }
        Screen::Help => {
            let lines = help_lines(width);
            // Compose's ^G opens on its section.
            if let Some(topic) = app.help_topic.take()
                && let Some(line) = lines.iter().position(|line| line.text().trim() == topic)
            {
                app.scroll.insert(PaneId::Help, line);
            }
            draw_pane(app, frame, PaneId::Help, body, lines, None);
        }
        Screen::Compose => compose::draw_compose(app, frame, theme, body),
        Screen::Postponed => {
            let lines = compose::postponed_lines(app, theme, width);
            let cursor = app.postponed_cursor + 2;
            draw_pane(app, frame, PaneId::Postponed, body, lines, Some(cursor));
        }
        Screen::Outbox => {
            let lines = compose::outbox_lines(app, theme, width);
            let cursor = compose::outbox_cursor_line(app);
            draw_pane(app, frame, PaneId::Outbox, body, lines, Some(cursor));
        }
        Screen::Attachments => {
            let lines = attachment_lines(app, theme, width);
            let cursor = app.attachments.as_ref().map(|list| list.cursor + 4);
            draw_pane(app, frame, PaneId::Attachments, body, lines, cursor);
        }
        Screen::Accounts => accounts::draw_accounts(app, frame, theme, body),
        Screen::AddAccount => accounts::draw_add(app, frame, theme, body),
        Screen::EditAccount if app.edit.as_ref().is_some_and(|edit| edit.form.is_some()) => {
            identities::draw_identity_form(app, frame, theme, body)
        }
        Screen::EditAccount => accounts::draw_edit(app, frame, theme, body),
    }
    if let Some(Prompt::Save(save)) = app.prompt.clone() {
        draw_save(app, frame, theme, body, &save);
    }
    if let Some(Prompt::Palette(palette)) = app.prompt.clone() {
        clear(frame, body);
        let lines = palette_lines(&palette, theme, width);
        app.reveal.insert(PaneId::Palette);
        draw_pane(
            app,
            frame,
            PaneId::Palette,
            body,
            lines,
            Some(palette.cursor + 3),
        );
    }
    draw_status(app, frame, theme, Rect::new(0, area.height - 3, width, 1));
    draw_commands(app, frame, theme, Rect::new(0, area.height - 2, width, 2));
}

/// The save prompt over the body: the folder list on `^T`, or what Tab
/// found on the body's last row.
fn draw_save(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect, save: &SavePrompt) {
    let width = usize::from(body.width);
    if let Some(cursor) = save.picker {
        clear(frame, body);
        let mut lines = vec![
            PaneLine::new(Span::styled(
                fit("  SAVE TO FOLDER", width, theme.ellipsis()),
                theme.bold().add_modifier(Modifier::UNDERLINED),
            )),
            PaneLine::default(),
        ];
        for (index, folder) in save.folders.iter().enumerate() {
            let style = if index == cursor {
                theme.reversed()
            } else {
                Style::new()
            };
            lines.push(PaneLine {
                target: Some(index),
                ..PaneLine::new(Span::styled(
                    fit(&format!("    {}", folder.name), width, theme.ellipsis()),
                    style,
                ))
            });
        }
        app.reveal.insert(PaneId::Picker);
        draw_pane(app, frame, PaneId::Picker, body, lines, Some(cursor + 2));
        return;
    }
    let Some(matches) = &save.matches else {
        return;
    };
    let row = Rect::new(
        body.x,
        body.y + body.height.saturating_sub(1),
        body.width,
        1,
    );
    clear(frame, row);
    let mut spans = vec![Span::styled(" Folders: ", theme.faint())];
    if matches.is_empty() {
        spans.push(Span::raw(format!(
            "none match \"{}\"",
            text::truncate(&save.input, width / 2, theme.ellipsis())
        )));
    }
    let mut used = text::width(" Folders: ");
    for (position, &index) in matches.iter().enumerate() {
        let name = &save.folders[index].name;
        let more = matches.len() - position;
        // Room for this name and, if more follow, a count of them.
        let tail = if more > 1 { 8 } else { 0 };
        if used + text::width(name) + 2 + tail > width && position > 0 {
            spans.push(Span::styled(format!("+{more} more"), theme.faint()));
            break;
        }
        let style = if save.cycle == Some(position) {
            theme.reversed()
        } else {
            theme.bold()
        };
        spans.push(Span::styled(name.clone(), style));
        spans.push(Span::raw("  "));
        used += text::width(name) + 2;
    }
    frame
        .buffer_mut()
        .set_line(row.x, row.y, &Line::from(spans), row.width);
}

/// The command palette over the body: each command that matches, best
/// first, with its key, its label and what it does, the letters that
/// matched in bold.
fn palette_lines(palette: &Palette, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit("  COMMANDS ON THIS SCREEN", width, theme.ellipsis()),
            theme.bold().add_modifier(Modifier::UNDERLINED),
        )),
        PaneLine::new(Span::styled(
            fit(
                "  Type to narrow the list. Enter runs the one under the cursor, Esc closes.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::default(),
    ];
    if palette.shown.is_empty() {
        lines.push(PaneLine::new(Span::raw(fit(
            &format!("  Nothing matches \"{}\"", palette.query),
            width,
            theme.ellipsis(),
        ))));
        return lines;
    }
    let label_width = palette
        .items
        .iter()
        .map(|item| item.entry.label.len())
        .max()
        .unwrap_or(0);
    for (row, (index, positions)) in palette.shown.iter().enumerate() {
        let item = &palette.items[*index];
        let label = item.entry.label;
        let prefix = format!("    {:<5}  ", item.entry.key);
        let line = format!("{prefix}{label:<label_width$}  {}", item.description);
        // Matched offsets are in "label description".
        let description_start = prefix.len() + label_width + 2;
        let marks: Vec<(Range<usize>, Style)> = positions
            .iter()
            .map(|&offset| {
                let at = if offset < label.len() {
                    prefix.len() + offset
                } else {
                    description_start + offset - label.len() - 1
                };
                let end = at + line[at..].chars().next().map_or(1, char::len_utf8);
                (at..end, theme.bold().add_modifier(Modifier::UNDERLINED))
            })
            .collect();
        let base = if row == palette.cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        let line = fit(&line, width, theme.ellipsis());
        lines.push(PaneLine {
            spans: styled(&line, base, &marks),
            target: Some(row),
            ..PaneLine::default()
        });
    }
    lines
}

/// Blanks `area`, for what is drawn over the body.
fn clear(frame: &mut Frame, area: Rect) {
    let buffer = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buffer.cell_mut(Position::new(x, y)) {
                cell.reset();
            }
        }
    }
}

/// Draws `lines` in `area` from the pane's scroll position, keeping
/// `cursor` in view when it moved, and the selection in reverse video.
fn draw_pane(
    app: &mut App,
    frame: &mut Frame,
    id: PaneId,
    area: Rect,
    lines: Vec<PaneLine>,
    cursor: Option<usize>,
) {
    let height = usize::from(area.height);
    let mut scroll = app.scroll.get(&id).copied().unwrap_or(0);
    if let Some(cursor) = cursor
        && app.reveal.remove(&id)
    {
        if cursor < scroll {
            scroll = cursor;
        } else if cursor >= scroll + height {
            scroll = cursor + 1 - height;
        }
    }
    scroll = scroll.min(lines.len().saturating_sub(height));
    app.scroll.insert(id, scroll);
    let buffer = frame.buffer_mut();
    for (row, line) in lines.iter().skip(scroll).take(height).enumerate() {
        buffer.set_line(
            area.x,
            area.y + row as u16,
            &Line::from(line.spans.clone()),
            area.width,
        );
    }
    if let Some(selection) = app.selection.filter(|selection| selection.pane == id) {
        for row in 0..height.min(lines.len().saturating_sub(scroll)) {
            for column in 0..usize::from(area.width) {
                if selection.contains(scroll + row, column)
                    && let Some(cell) =
                        buffer.cell_mut(Position::new(area.x + column as u16, area.y + row as u16))
                {
                    cell.modifier.toggle(Modifier::REVERSED);
                }
            }
        }
    }
    app.panes.push(DrawnPane {
        id,
        area,
        scroll,
        lines,
    });
}

fn draw_title(app: &App, frame: &mut Frame, theme: Theme, area: Rect) {
    let width = usize::from(area.width);
    let buffer = frame.buffer_mut();
    buffer.set_style(area, theme.reversed());
    let left = format!("  ZMAIL {}", app.version);
    let name = match app.screen {
        Screen::MainMenu => "MAIN MENU",
        Screen::FolderList => "FOLDER LIST",
        Screen::Index => "MESSAGE INDEX",
        Screen::Message => "MESSAGE TEXT",
        Screen::Setup => "SETUP",
        Screen::Help => "HELP",
        Screen::Compose => "COMPOSE MESSAGE",
        Screen::Postponed => "POSTPONED MESSAGES",
        Screen::Outbox => "OUTBOX",
        Screen::Attachments => "ATTACHMENT INDEX",
        Screen::Accounts => "SETUP ACCOUNTS",
        Screen::AddAccount => "ADD ACCOUNT",
        Screen::EditAccount => match app.edit.as_ref().and_then(|edit| edit.form.as_ref()) {
            Some(form) if form.mode == zmail_setup::identity_editor::FormMode::Add => {
                "ADD IDENTITY"
            }
            Some(_) => "CHANGE IDENTITY",
            None => "CHANGE ACCOUNT",
        },
    };
    let detail = app.title_detail();
    let right = fit_right(
        &detail,
        width.saturating_sub(text::width(&left) + 2),
        theme.ellipsis(),
    );
    buffer.set_stringn(area.x, area.y, &left, width, theme.reversed());
    let center = (width.saturating_sub(name.len())) / 2;
    if center > text::width(&left) + 1
        && center + name.len() + text::width(right.trim_start()) + 3 < width
    {
        buffer.set_string(area.x + center as u16, area.y, name, theme.reversed());
    }
    let right = format!("{} ", right.trim_start());
    let x = width.saturating_sub(text::width(&right));
    buffer.set_stringn(
        area.x + x as u16,
        area.y,
        &right,
        width - x,
        theme.reversed(),
    );
}

fn draw_status(app: &App, frame: &mut Frame, theme: Theme, area: Rect) {
    let width = usize::from(area.width);
    if let Some(prompt) = app.prompt_text() {
        let field = matches!(
            app.prompt,
            Some(
                Prompt::Text { .. }
                    | Prompt::Save(SavePrompt { picker: None, .. })
                    | Prompt::Path(_)
                    | Prompt::Palette(_)
            )
        );
        // A field shows its end, where the typing is.
        let shown = if field {
            text::truncate_start(&prompt, width.saturating_sub(1), theme.ellipsis())
        } else {
            text::truncate(&prompt, width.saturating_sub(1), theme.ellipsis())
        };
        frame
            .buffer_mut()
            .set_stringn(area.x, area.y, &shown, width, theme.bold());
        if field {
            let x = text::width(&shown).min(width.saturating_sub(1));
            frame.set_cursor_position(Position::new(area.x + x as u16, area.y));
        }
        return;
    }
    let note = app.sync_note().unwrap_or_default();
    let note_width = text::width(&note);
    if let Some(status) = &app.status {
        let room = width.saturating_sub(if note.is_empty() { 0 } else { note_width + 2 });
        let shown = text::truncate(&format!("[{status}]"), room, theme.ellipsis());
        let x = (room.saturating_sub(text::width(&shown))) / 2;
        frame
            .buffer_mut()
            .set_stringn(area.x + x as u16, area.y, &shown, room, theme.reversed());
    }
    if !note.is_empty() && note_width < width {
        frame.buffer_mut().set_stringn(
            area.x + (width - note_width) as u16,
            area.y,
            &note,
            note_width,
            theme.faint(),
        );
    }
}

fn draw_commands(app: &mut App, frame: &mut Frame, theme: Theme, area: Rect) {
    let entries = app.command_bar(area.width);
    let columns = usize::from(area.width / 13).clamp(2, 8);
    let slot = usize::from(area.width) / columns;
    for (index, entry) in entries.iter().enumerate() {
        let x = area.x + ((index / 2) * slot) as u16;
        let y = area.y + (index % 2) as u16;
        let buffer = frame.buffer_mut();
        let key = fit(entry.key, entry.key.len(), "");
        buffer.set_stringn(x, y, &key, slot, theme.reversed());
        let label = fit(
            entry.label,
            slot.saturating_sub(key.len() + 1),
            theme.ellipsis(),
        );
        buffer.set_stringn(x + key.len() as u16 + 1, y, &label, slot, Style::new());
        app.command_targets
            .push((Rect::new(x, y, slot as u16, 1), entry.cmd));
    }
}

fn menu_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let margin = " ".repeat(width.saturating_sub(70) / 2 + 2);
    let mut lines = vec![PaneLine::default(), PaneLine::default()];
    for (index, (key, name, description, _)) in MENU_ITEMS.iter().enumerate() {
        let row = fit(
            &format!("{margin}{key:<6}{name:<19}-  {description}"),
            width,
            theme.ellipsis(),
        );
        let style = if index == app.menu_cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(row, style))
        });
    }
    lines.push(PaneLine::default());
    if let Some(folder) = &app.folder {
        lines.push(PaneLine::new(Span::styled(
            fit(
                &format!("{margin}Current folder: {}", folder.name),
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )));
    }
    lines
}

fn folder_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    if app.folders.is_empty() {
        return vec![
            PaneLine::default(),
            PaneLine::new(Span::raw(fit(
                "  No accounts yet. Add one in the Zmail window, or with zmail-app add-imap or \
                 add-jmap.",
                width,
                theme.ellipsis(),
            ))),
        ];
    }
    let current = app.folder.as_ref().map(|folder| folder.selection);
    app.folders
        .iter()
        .enumerate()
        .map(|(index, row)| match row {
            FolderRow::Collection(title) => PaneLine::new(Span::styled(
                fit(title, width, theme.ellipsis()),
                theme.bold().add_modifier(Modifier::UNDERLINED),
            )),
            FolderRow::Gap => PaneLine::default(),
            FolderRow::Local(local) => {
                let (name, counts) = match *local {
                    LocalFolder::Postponed { count } => ("Postponed", format!("{count:>6}")),
                    LocalFolder::Outbox { count, failed: 0 } => ("Outbox", format!("{count:>6}")),
                    LocalFolder::Outbox { count, failed } => {
                        ("Outbox", format!("{failed} failed  {count:>6}"))
                    }
                };
                let counts_width = 16.min(width / 3);
                let row = format!(
                    "{}{}",
                    fit(
                        &format!("    {name}"),
                        width - counts_width,
                        theme.ellipsis()
                    ),
                    fit_right(&counts, counts_width, theme.ellipsis())
                );
                let style = if index == app.folder_cursor {
                    theme.reversed()
                } else {
                    Style::new()
                };
                PaneLine {
                    target: Some(index),
                    ..PaneLine::new(Span::styled(row, style))
                }
            }
            FolderRow::Folder(folder) => {
                let counts = if folder.unread > 0 {
                    format!("{} new  {:>6}", folder.unread, folder.total)
                } else {
                    format!("{:>6}", folder.total)
                };
                let marker = if Some(folder.selection) == current {
                    "* "
                } else {
                    "  "
                };
                let name = format!("{marker}{}{}", "  ".repeat(folder.depth + 1), folder.name);
                let counts_width = 16.min(width / 3);
                let row = format!(
                    "{}{}",
                    fit(&name, width - counts_width, theme.ellipsis()),
                    fit_right(&counts, counts_width, theme.ellipsis())
                );
                let mut style = if folder.unread > 0 {
                    theme.bold()
                } else {
                    Style::new()
                };
                if index == app.folder_cursor {
                    style = style.patch(theme.reversed());
                }
                PaneLine {
                    target: Some(index),
                    ..PaneLine::new(Span::styled(row, style))
                }
            }
        })
        .collect()
}

fn draw_index(app: &mut App, frame: &mut Frame, theme: Theme, area: Rect) {
    let lines = index_lines(app, theme, area.width);
    let cursor = app.index.cursor;
    draw_pane(app, frame, PaneId::Index, area, lines, Some(cursor));
}

fn index_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    if app.index.entries.is_empty() {
        let note = match (&app.folder, &app.index.search) {
            (None, _) => "  Choose a folder in the Folder List (L).",
            (_, Some(_)) => "  Nothing matches. Esc or < shows the whole folder.",
            _ => "  There are no messages in this folder.",
        };
        return vec![
            PaneLine::default(),
            PaneLine::new(Span::raw(fit(note, width, theme.ellipsis()))),
        ];
    }
    let now = app.now();
    let sender_width = (width / 4).clamp(8, 24);
    let fixed = MARKER_WIDTH + 3 + 1 + 9 + 2 + sender_width + 1;
    let subject_width = width.saturating_sub(fixed);
    app.index
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let selected = if app.selected.contains(&entry.thread_id) {
                "X "
            } else {
                "  "
            };
            let marks = format!(
                "{}{}{}",
                if entry.unread { 'N' } else { ' ' },
                if entry.flagged { '*' } else { ' ' },
                if entry.attachments { '@' } else { ' ' },
            );
            let date = format_list_date(&entry.date, &now, app.hour_cycle);
            let subject = if entry.count > 1 && entry.message_id.is_none() {
                format!("({}) {}", entry.count, entry.subject)
            } else {
                entry.subject.clone()
            };
            let rest = format!(
                " {}  {} {}",
                fit_right(&date, 9, theme.ellipsis()),
                fit(&entry.sender, sender_width, theme.ellipsis()),
                fit(&subject, subject_width, theme.ellipsis())
            );
            let mut base = if entry.unread {
                theme.bold()
            } else {
                Style::new()
            };
            let mut flag = if entry.flagged { theme.flag() } else { base };
            let mut marker = theme.bold();
            if index == app.index.cursor {
                base = base.patch(theme.reversed());
                flag = flag.patch(theme.reversed());
                marker = marker.patch(theme.reversed());
            }
            PaneLine {
                spans: vec![
                    Span::styled(selected.to_owned(), marker),
                    Span::styled(marks[..1].to_owned(), base),
                    Span::styled(marks[1..2].to_owned(), flag),
                    Span::styled(marks[2..].to_owned(), base),
                    Span::styled(
                        text::truncate(&rest, width.saturating_sub(MARKER_WIDTH + 3), ""),
                        base,
                    ),
                ],
                target: Some(index),
                ..PaneLine::default()
            }
        })
        .collect()
}

fn draw_message(app: &mut App, frame: &mut Frame, theme: Theme, area: Rect) {
    let looks_safe = app.looks_safe_key();
    let Some(state) = app.message.as_mut() else {
        return;
    };
    let (lines, starts) = message_lines(
        &state.conversation,
        area.width,
        theme,
        state.selected_link,
        state.find.as_deref(),
        Some(&looks_safe),
    );
    state.drawn_warnings = state
        .conversation
        .warnings
        .iter()
        .filter_map(|(id, doc_line)| Some((*id, *starts.get(*doc_line)?)))
        .collect();
    state.drawn_starts = state
        .conversation
        .message_starts
        .iter()
        .filter_map(|(id, doc_line)| Some((*id, *starts.get(*doc_line)?)))
        .collect();
    // A single message opened from the index starts at its headers.
    if !state.focused {
        state.focused = true;
        if let Some(line) = state
            .focus
            .and_then(|focus| {
                state
                    .conversation
                    .message_starts
                    .iter()
                    .find(|(id, _)| *id == focus)
            })
            .and_then(|(_, doc_line)| starts.get(*doc_line))
        {
            // Show the "Message n of m" rule above it too.
            app.scroll.insert(PaneId::Message, line.saturating_sub(1));
        }
    }
    // Tab moved to a link: bring it into view.
    let link_line = state.selected_link.and_then(|link| {
        lines
            .iter()
            .position(|line| line.links.iter().any(|(_, index)| *index == link))
    });
    let reveal = app.reveal.contains(&PaneId::Message);
    draw_pane(
        app,
        frame,
        PaneId::Message,
        area,
        lines,
        if reveal { link_line } else { None },
    );
    app.reveal.remove(&PaneId::Message);
}

/// The lines of `conversation` wrapped to `width`, and the first line of
/// each of its document lines. A sender-safety warning ends with the key
/// that hides it, `looks_safe`, where that key works.
pub(crate) fn message_lines(
    conversation: &Conversation,
    width: u16,
    theme: Theme,
    selected_link: Option<usize>,
    find: Option<&str>,
    looks_safe: Option<&str>,
) -> (Vec<PaneLine>, Vec<usize>) {
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut starts = Vec::with_capacity(conversation.lines.len());
    let find = find
        .map(str::to_ascii_lowercase)
        .filter(|find| !find.is_empty());
    for doc_line in &conversation.lines {
        starts.push(lines.len());
        match doc_line {
            DocLine::Blank => lines.push(PaneLine::default()),
            DocLine::Rule(title) => {
                let head = format!("{h}{h} {title} ", h = theme.horizontal());
                let fill = theme
                    .horizontal()
                    .repeat(width.saturating_sub(text::width(&head)));
                lines.push(PaneLine::new(Span::styled(
                    fit(&format!("{head}{fill}"), width, theme.ellipsis()),
                    theme.faint(),
                )));
            }
            DocLine::Note(note) => {
                for row in wrap(&format!("[{note}]"), width) {
                    lines.push(PaneLine {
                        joiner: row.joiner,
                        ..PaneLine::new(Span::styled(
                            row.text,
                            theme.faint().add_modifier(Modifier::ITALIC),
                        ))
                    });
                }
            }
            DocLine::Warning { tier, text } => {
                let style = theme.warning(*tier);
                for row in wrap(&format!("! {text}"), width) {
                    lines.push(PaneLine {
                        joiner: row.joiner,
                        ..PaneLine::new(Span::styled(row.text, style))
                    });
                }
            }
            DocLine::WarningDetail(detail) => {
                for row in wrap(&format!("  {detail}"), width) {
                    lines.push(PaneLine {
                        joiner: row.joiner,
                        ..PaneLine::new(Span::raw(row.text))
                    });
                }
            }
            DocLine::LooksSafe => {
                if let Some(key) = looks_safe {
                    lines.push(PaneLine {
                        spans: vec![
                            Span::raw("  "),
                            Span::styled(key.to_owned(), theme.reversed()),
                            Span::styled(
                                fit(
                                    " Looks safe: hide this warning for this message",
                                    width.saturating_sub(2 + text::width(key)),
                                    theme.ellipsis(),
                                ),
                                theme.faint(),
                            ),
                        ],
                        ..PaneLine::default()
                    });
                }
            }
            DocLine::Header { label, value } => {
                let line = format!("{label}: {value}");
                let label_end = label.len() + 1;
                for row in wrap(&line, width) {
                    let mut marks = Vec::new();
                    if row.start < label_end {
                        marks.push((0..(label_end - row.start).min(row.text.len()), theme.bold()));
                    }
                    marks.extend(found(&row.text, find.as_deref(), theme));
                    lines.push(PaneLine {
                        spans: styled(&row.text, Style::new(), &marks),
                        joiner: row.joiner,
                        ..PaneLine::default()
                    });
                }
            }
            DocLine::Text { text, links } => {
                for row in wrap(text, width) {
                    let row_end = row.start + row.text.len();
                    let mut marks = Vec::new();
                    let mut cells = Vec::new();
                    for (range, link) in links {
                        if range.start >= row.start && range.end <= row_end {
                            let local = range.start - row.start..range.end - row.start;
                            let style = if selected_link == Some(*link) {
                                theme.link().add_modifier(Modifier::REVERSED)
                            } else {
                                theme.link()
                            };
                            cells.push((
                                column_of(&row.text, local.start)..column_of(&row.text, local.end),
                                *link,
                            ));
                            marks.push((local, style));
                        }
                    }
                    marks.extend(found(&row.text, find.as_deref(), theme));
                    lines.push(PaneLine {
                        spans: styled(&row.text, Style::new(), &marks),
                        joiner: row.joiner,
                        target: None,
                        links: cells,
                    });
                }
            }
        }
    }
    (lines, starts)
}

/// The byte ranges of `find` in `text`, ignoring ASCII case.
fn found(text: &str, find: Option<&str>, theme: Theme) -> Vec<(Range<usize>, Style)> {
    let Some(find) = find else {
        return Vec::new();
    };
    let lower = text.to_ascii_lowercase();
    lower
        .match_indices(find)
        .map(|(start, matched)| (start..start + matched.len(), theme.found()))
        .collect()
}

/// `text` as spans: `base`, with each mark's style laid over its byte
/// range.
fn styled(text: &str, base: Style, marks: &[(Range<usize>, Style)]) -> Vec<Span<'static>> {
    let mut cuts: Vec<usize> = marks
        .iter()
        .flat_map(|(range, _)| [range.start, range.end])
        .chain([0, text.len()])
        .filter(|&cut| cut <= text.len() && text.is_char_boundary(cut))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter(|pair| pair[0] < pair[1])
        .map(|pair| {
            let style = marks
                .iter()
                .filter(|(range, _)| range.start <= pair[0] && pair[1] <= range.end)
                .fold(base, |style, (_, mark)| style.patch(*mark));
            Span::styled(text[pair[0]..pair[1]].to_owned(), style)
        })
        .collect()
}

/// The Attachment Index: what it is for, a header, then each attachment's
/// number, size, type and name (filtered when read), with the message it
/// came with when the conversation has several.
fn attachment_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let Some(list) = &app.attachments else {
        return Vec::new();
    };
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit(
                &format!("  Attachments in \"{}\"", list.subject),
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::new(Span::styled(
            fit(
                "  S saves one, A saves them all into a folder. Zmail doesn't open them here.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::default(),
    ];
    let type_width = (width / 5).clamp(10, 28);
    // "Msg 2  " and the sender, when the conversation has several messages
    // and there is room.
    let from_width = if list.messages > 1 && width >= 100 {
        (width / 4).min(32)
    } else {
        0
    };
    let fixed = 2 + 3 + 2 + 8 + 2 + type_width + 2;
    let name_width = width.saturating_sub(fixed + from_width).max(8);
    let row = |number: &str, size: &str, kind: &str, name: &str, from: &str| {
        let mut row = format!(
            "  {number:>3}  {}  {}  {}",
            fit_right(size, 8, theme.ellipsis()),
            fit(kind, type_width, theme.ellipsis()),
            fit(name, name_width, theme.ellipsis()),
        );
        if from_width > 0 {
            row.push_str(&fit(&format!("  {from}"), from_width, theme.ellipsis()));
        }
        fit(&row, width, theme.ellipsis())
    };
    lines.push(PaneLine::new(Span::styled(
        row("#", "Size", "Type", "Name", "From"),
        theme.bold(),
    )));
    for (index, attachment) in list.rows.iter().enumerate() {
        let style = if index == list.cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                row(
                    &(index + 1).to_string(),
                    &crate::message::size_label(attachment.size),
                    &attachment.content_type,
                    &attachment.name,
                    &format!("Msg {}: {}", attachment.message, attachment.sender),
                ),
                style,
            ))
        });
    }
    lines
}

fn setup_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let check = |on: bool| if on { "[X]" } else { "[ ]" };
    let prefs = &app.prefs;
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit(
                "  The terminal's settings, kept in the encrypted mail store. Enter changes one.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::new(Span::styled(
            fit(
                "  A: your mail accounts, to add, change or delete one.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::default(),
    ];
    for (index, row) in SETUP_ROWS.iter().enumerate() {
        let text = match row {
            SetupRow::Mouse => format!(
                "{} Use the mouse: click, wheel, drag to select and copy",
                check(prefs.mouse)
            ),
            SetupRow::Bell => format!("{} Ring the bell when new mail arrives", check(prefs.bell)),
            SetupRow::Preview => format!(
                "{} Preview pane beside the Message Index (120 columns or wider)",
                check(prefs.preview_pane)
            ),
            SetupRow::Threads => format!(
                "{} Group the Message Index by conversation",
                check(prefs.group_threads)
            ),
            SetupRow::Ascii => format!("{} ASCII only, no box drawing", check(prefs.ascii)),
            SetupRow::Clipboard => format!(
                "    OSC 52 copies to the {}",
                match prefs.copy_clipboard {
                    CopyClipboard::System => "system clipboard",
                    CopyClipboard::Primary => "primary selection",
                }
            ),
            SetupRow::CopyCommand => format!(
                "    Copy command: {}",
                prefs.copy_command.as_deref().unwrap_or("none")
            ),
            SetupRow::ExternalEditor => format!(
                "{} Edit the message text in $VISUAL or $EDITOR (^_ in Compose)",
                check(prefs.external_editor)
            ),
        };
        let style = if index == app.setup_cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(&format!("  {text}"), width, theme.ellipsis()),
                style,
            ))
        });
    }
    let target = match copy_target(prefs, app.env.macos, app.env.over_ssh) {
        CopyTarget::Osc52(CopyClipboard::System) => {
            "OSC 52, to the system clipboard (your terminal must allow it)".to_owned()
        }
        CopyTarget::Osc52(CopyClipboard::Primary) => {
            "OSC 52, to the primary selection (your terminal must allow it)".to_owned()
        }
        CopyTarget::Command(command) => format!("the command {}", command.join(" ")),
    };
    lines.push(PaneLine::default());
    for note in [
        format!("  Copies go to {target}."),
        "  Hold Shift while dragging to use the terminal's own selection instead.".to_owned(),
    ] {
        lines.push(PaneLine::new(Span::styled(
            fit(&note, width, theme.ellipsis()),
            theme.faint(),
        )));
    }
    // Said plainly, wrapped rather than cut off (ADR 0007, section 6).
    lines.push(PaneLine::default());
    let warning = "The external editor writes the draft unencrypted to a temporary file \
        while the editor is open. The file is in a new folder only you can open, and Zmail \
        overwrites and removes it when the editor exits. Leave this off to keep drafts only \
        in the encrypted store.";
    let style = if prefs.external_editor {
        theme.bold()
    } else {
        theme.faint()
    };
    for (index, row) in wrap(warning, width.saturating_sub(8).max(10))
        .into_iter()
        .enumerate()
    {
        let prefix = if index == 0 { "  Note: " } else { "        " };
        lines.push(PaneLine {
            joiner: row.joiner,
            ..PaneLine::new(Span::styled(format!("{prefix}{}", row.text), style))
        });
    }
    lines
}

const HELP: &str = "\
ZMAIL IN THE TERMINAL

Zmail's terminal screens work like Pine and Alpine: the keys that work on a \
screen are always listed at the bottom, and O shows more of them. Letters work \
in either case.

Moving around
  M          Main Menu, from anywhere
  L          Folder List: each account is a collection of folders
  I          Message Index of the current folder
  < or Left  up a level;  > Enter or Right  open what the cursor is on
  P N        previous and next item (Up and Down work too)
  - Space    previous and next page;  Home End  first and last
  W          WhereIs: search the Message Index with the search language \
(from:, to:, subject:, has:attachment, is:unread, before:, after:), find a \
folder, or find text in a message. The index follows as you type; Enter also \
asks the server for mail not downloaded yet.
  /          list conversations or single messages
  V          the preview pane, beside the index in terminals 120 columns or wider
  :          the command palette: every command of the screen with its key; \
type a few letters of what you want (arch, folder, trash), Up and Down move, \
Enter runs it, Esc closes. Not in Compose or the account forms, where : is \
text.
  Q          quit (Zmail asks first)

Triage
  These work in the Message Index on the message under the cursor, and in \
Message Text on the open one. A command acts on the whole conversation, as \
in the Zmail window, also when the index lists single messages.
  D          delete: to the Trash (for good when the account has none)
  E          archive
  S          save to a folder. Tab completes the folder's name, again to go \
through the matches; Ctrl-T lists the folders to pick from.
  *          flag, or unflag
  K          mark read, or unread
  U or Z     undo the last of these, wherever it was done; the status line \
says what each did
  ;          select the message under the cursor, or not (X in the first \
column; a click there does it too)
  A          apply the next command (D, E, S, * or K) to every selected \
message; ; there unselects them all
  A message stays unread in Message Text until it has been open for a moment.

Reading
  Message Text shows the text of each message. Links are numbered, like [3]; \
Tab and Shift-Tab move between them, and Enter (or a click) shows the real \
address before anything opens it. Remote images and HTML formatting are not \
shown in the terminal.
  A message whose sender looks suspicious gets the Zmail window's warning \
above its text: a sender's domain that imitates a well-known one, or replies \
that go to another domain than the sender's, with the addresses it is based \
on. Settings in the Zmail window can leave out the mildest (a related \
domain).
  X          Looks safe: hide the warning in view, for that message only \
(the Zmail window hides it too)
  V          the Attachment Index: the conversation's attachments, with \
their names, types and sizes
  S or Enter there saves the attachment under the cursor. The path starts \
in the folder you last saved to (else Downloads, else your home folder) with \
the attachment's name made safe for a file; Tab completes it, and a folder \
saves into that folder. Zmail asks before replacing a file.
  A there saves every attachment into a folder, numbering a name that is \
taken (report (1).pdf).
  Zmail never opens an attachment from the terminal: save it, then open it \
yourself.

Writing mail
  C          compose, from anywhere it is free. When messages are postponed, \
Zmail asks first whether to continue one.
  R          reply to the message (in Message Text, the one you are reading); \
when others got it too, Zmail asks whether to reply to all
  F          forward the message, with its text
  T          the Outbox, from the Main Menu (or Outbox in the Folder List): \
messages waiting to send or that couldn't be sent. R sends a failed one \
again; D takes back one that hasn't started sending.
  Compose works like Pico. Up and Down, Enter and Tab move between the \
fields; Tab in To, Cc and Bcc completes an address you have used before, \
again to go through the matches.
  ^X         send (Zmail asks first). The message waits for the undo-send \
delay set in the Zmail window; until then U takes it back and opens it again
  ^O         postpone: keep the draft for later
  ^C         cancel: deletes the draft, after asking if you changed it
  ^K ^U      cut the line (the field in a header, an attachment on \
Attchmnt), and put back what was cut
  ^J or ^A   attach a file: type its path, with Tab completion, or ^T to \
browse
  ^R         insert a file's text into the message
  ^W         find text in the message
  ^T         your addresses on To, Cc and Bcc; the file browser on Attchmnt
  ^G         this help
  ^_         the message text in your own editor ($VISUAL or $EDITOR; \
many terminals send ^_ for Ctrl-7 or Ctrl-/ too), only once Setup turns it \
on: the draft is then unencrypted in a temporary file while the editor is \
open, and removed when it exits
  Drafts are saved in the encrypted mail store as you write, and sent by \
whichever Zmail syncs.

Copy and paste
  Drag with the mouse to select text in one pane; it is copied when you let \
go. Double-click selects a word, triple-click a line, and dragging past the \
top or bottom scrolls. Without a mouse: Ctrl-Space sets a mark in Message \
Text, the arrow keys select, and y or Ctrl-C copies.
  Copies go to the clipboard with OSC 52, which works locally and over SSH in \
most terminals (tmux needs set-clipboard on). On a Mac outside SSH they go \
through pbcopy. Setup (S on the Main Menu) can set a copy command instead, such \
as wl-copy or xclip -selection clipboard, or turn the mouse off.
  Hold Shift while dragging to use the terminal's own selection.
  Pasting works in text fields such as WhereIs and in Compose, where it \
goes in as text (a list pasted into To splits into addresses); anywhere \
else a paste is ignored, so pasted text can never run commands.

Accounts
  S on the Main Menu, then A: your mail accounts (Alpine's collection \
list). A adds one, C changes the one under the cursor (a new password, \
signing in again in the browser, the cache budget), D deletes it from this \
computer after asking; its mail on the server stays.
  Adding asks for your email address first. Zmail uses its own settings for \
Gmail, Outlook.com, Fastmail and iCloud addresses, and otherwise looks the \
domain up, step by step, asking before it sends the domain to Mozilla's \
provider database. Then how you sign in: a password, an app password, or \
OAuth in the browser with a client of your own (Zmail ships none). Zmail \
tests the sign-in before saving, then asks for the name and address mail \
goes out with.
  In the form, Up, Down and Tab move between the fields, Enter goes to the \
next one or continues from the last, Space changes a choice, ^U clears a \
field, Esc goes back a step and ^C cancels. Password fields show [hidden], \
never what you type, and the password goes only to the system keychain.
  OAuth opens your browser and also shows the address to open by hand. The \
browser sends the sign-in back to 127.0.0.1 on its own computer, so over SSH \
it can't reach this Zmail: use an app password, or add the account on the \
computer you're at.
  A new account starts syncing here when this Zmail syncs, otherwise in the \
Zmail window that does.

Sync
  One Zmail syncs your mail at a time. When another Zmail window already \
does, this one shows the mail it syncs, and the status line says so.";

fn help_lines(width: u16) -> Vec<PaneLine> {
    let margin = "  ";
    let width = usize::from(width).saturating_sub(margin.len()).max(20);
    let mut lines = Vec::new();
    for line in HELP.split('\n') {
        // A key's description wraps under itself: "  W          WhereIs…".
        let hang = line
            .strip_prefix("  ")
            .and_then(|rest| rest.find("  ").map(|key| (rest, key)))
            .and_then(|(rest, key)| {
                let text = key + rest[key..].len() - rest[key..].trim_start().len();
                (text < 16).then_some(text + 2)
            })
            .unwrap_or(0);
        let rows = wrap(line, width);
        let first = rows
            .first()
            .cloned()
            .unwrap_or_else(|| wrap("", width).remove(0));
        lines.push(PaneLine::new(Span::raw(format!("{margin}{}", first.text))));
        let rest = line[(first.start + first.text.len()).min(line.len())..].trim_start();
        if rest.is_empty() {
            continue;
        }
        for row in wrap(rest, width.saturating_sub(hang).max(10)) {
            lines.push(PaneLine {
                joiner: Some(" "),
                ..PaneLine::new(Span::raw(format!(
                    "{margin}{}{}",
                    " ".repeat(hang),
                    row.text
                )))
            });
        }
    }
    lines
}
