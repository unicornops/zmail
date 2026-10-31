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

use crate::{
    app::{App, MENU_ITEMS, SETUP_ROWS, Screen, SetupRow},
    clipboard::{CopyTarget, copy_target},
    data::FolderRow,
    message::{Conversation, DocLine},
    pane::{DrawnPane, PaneId, PaneLine},
    text::{self, column_of, fit, fit_right, wrap},
};

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
                let lines = match app.preview_conversation() {
                    Some(conversation) => {
                        message_lines(conversation, preview_area.width, theme, None, None).0
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
            let cursor = app.setup_cursor + 2;
            draw_pane(app, frame, PaneId::Setup, body, lines, Some(cursor));
        }
        Screen::Help => {
            let lines = help_lines(width);
            draw_pane(app, frame, PaneId::Help, body, lines, None);
        }
    }
    draw_status(app, frame, theme, Rect::new(0, area.height - 3, width, 1));
    draw_commands(app, frame, theme, Rect::new(0, area.height - 2, width, 2));
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
        let shown = text::truncate(&prompt, width.saturating_sub(1), theme.ellipsis());
        frame
            .buffer_mut()
            .set_stringn(area.x, area.y, &shown, width, theme.bold());
        if matches!(app.prompt, Some(crate::app::Prompt::Text { .. })) {
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
    let fixed = 3 + 1 + 9 + 2 + sender_width + 1;
    let subject_width = width.saturating_sub(fixed);
    app.index
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
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
            if index == app.index.cursor {
                base = base.patch(theme.reversed());
                flag = flag.patch(theme.reversed());
            }
            PaneLine {
                spans: vec![
                    Span::styled(marks[..1].to_owned(), base),
                    Span::styled(marks[1..2].to_owned(), flag),
                    Span::styled(marks[2..].to_owned(), base),
                    Span::styled(text::truncate(&rest, width - 3, ""), base),
                ],
                target: Some(index),
                ..PaneLine::default()
            }
        })
        .collect()
}

fn draw_message(app: &mut App, frame: &mut Frame, theme: Theme, area: Rect) {
    let Some(state) = app.message.as_mut() else {
        return;
    };
    let (lines, starts) = message_lines(
        &state.conversation,
        area.width,
        theme,
        state.selected_link,
        state.find.as_deref(),
    );
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
/// each of its document lines.
pub(crate) fn message_lines(
    conversation: &Conversation,
    width: u16,
    theme: Theme,
    selected_link: Option<usize>,
    find: Option<&str>,
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
  Q          quit (Zmail asks first)

Reading
  Message Text shows the text of each message. Links are numbered, like [3]; \
Tab and Shift-Tab move between them, and Enter (or a click) shows the real \
address before anything opens it. Remote images and HTML formatting are not \
shown in the terminal.

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
  Pasting works in text fields such as WhereIs; anywhere else a paste is \
ignored, so pasted text can never run commands.

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
