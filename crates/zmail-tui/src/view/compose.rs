//! Drawing the Compose screen, Pico's: a header field per row (From when
//! the account has several identities, To, Cc, Bcc, Attchmnt with an
//! attachment per row, Subject), the "Message Text" rule, then the body;
//! and the postponed messages and the Outbox.
//!
//! Draft text was filtered when the draft was opened or typed in
//! (`compose.rs`), file and address names when they were listed.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use zmail_core::time::format_list_date;

use super::{Theme, clear, draw_pane};
use crate::{
    app::App,
    compose::{Field, PickerKind, attachment_label, identity_label},
    outbox::OutboxRow,
    pane::{PaneId, PaneLine},
    text::{self, fit, fit_right},
};

/// Width of the header labels, `Attchmnt: `.
const LABEL_WIDTH: u16 = 10;

pub(super) fn draw_compose(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    app.compose_targets.clear();
    if app
        .compose
        .as_ref()
        .is_some_and(|compose| compose.picker.is_some())
    {
        return draw_picker(app, frame, theme, body);
    }
    let Some(compose) = app.compose.as_mut() else {
        return;
    };
    let width = body.width;
    let bottom = body.y + body.height;
    let mut y = body.y;
    let field_width = width.saturating_sub(LABEL_WIDTH);
    let cursor_style = |focused: bool| {
        if focused {
            theme.reversed()
        } else {
            Style::new()
        }
    };
    for field in compose.fields() {
        if field == Field::Body || y >= bottom {
            break;
        }
        let focused = compose.focus == field;
        let label_style = if focused { theme.bold() } else { Style::new() };
        let label = format!("{:<8}: ", field.label());
        match field {
            Field::From => {
                frame
                    .buffer_mut()
                    .set_stringn(0, y, &label, usize::from(width), label_style);
                let identity = compose
                    .identities
                    .iter()
                    .find(|identity| identity.id == compose.identity_id)
                    .map(identity_label)
                    .unwrap_or_default();
                let style = if focused {
                    theme.reversed()
                } else {
                    Style::new()
                };
                let mut spans = vec![Span::styled(identity, style)];
                if focused {
                    spans.push(Span::styled(
                        "  Space or Left/Right change it",
                        theme.faint(),
                    ));
                }
                frame
                    .buffer_mut()
                    .set_line(LABEL_WIDTH, y, &Line::from(spans), field_width);
                app.compose_targets
                    .push((Rect::new(0, y, width, 1), field, None));
                y += 1;
            }
            Field::Attachments => {
                frame
                    .buffer_mut()
                    .set_stringn(0, y, &label, usize::from(width), label_style);
                if compose.attachments.is_empty() {
                    if focused {
                        frame.buffer_mut().set_stringn(
                            LABEL_WIDTH,
                            y,
                            format!(
                                "none: {} or typing a path attaches a file",
                                crate::compose::ATTACH_KEY
                            ),
                            usize::from(field_width),
                            theme.faint(),
                        );
                    }
                    app.compose_targets
                        .push((Rect::new(0, y, width, 1), field, None));
                    y += 1;
                }
                for (index, attachment) in compose.attachments.iter().enumerate() {
                    if y >= bottom {
                        break;
                    }
                    let style = if focused && index == compose.attachment_cursor {
                        theme.reversed()
                    } else {
                        Style::new()
                    };
                    frame.buffer_mut().set_stringn(
                        LABEL_WIDTH,
                        y,
                        fit(
                            &format!("{}. {}", index + 1, attachment_label(attachment)),
                            usize::from(field_width),
                            theme.ellipsis(),
                        ),
                        usize::from(field_width),
                        style,
                    );
                    app.compose_targets
                        .push((Rect::new(0, y, width, 1), field, Some(index)));
                    y += 1;
                }
            }
            _ => {
                frame
                    .buffer_mut()
                    .set_stringn(0, y, &label, usize::from(width), label_style);
                if let Some(editor) = compose.editor_mut(field) {
                    editor.set_cursor_style(cursor_style(focused));
                    frame.render_widget(&*editor, Rect::new(LABEL_WIDTH, y, field_width, 1));
                }
                app.compose_targets
                    .push((Rect::new(0, y, width, 1), field, None));
                y += 1;
            }
        }
    }
    if y < bottom {
        let rule = "----- Message Text -----";
        frame
            .buffer_mut()
            .set_stringn(0, y, rule, usize::from(width), theme.faint());
        y += 1;
    }
    if y < bottom {
        let area = Rect::new(0, y, width, bottom - y);
        let focused = compose.focus == Field::Body;
        compose.body.set_cursor_style(cursor_style(focused));
        frame.render_widget(&compose.body, area);
        app.compose_targets.push((area, Field::Body, None));
    }
    // What Tab found, on the last row, as the save prompt shows folders.
    if let Some(completion) = &compose.completion {
        let row = Rect::new(body.x, bottom.saturating_sub(1), width, 1);
        clear(frame, row);
        let mut spans = vec![Span::styled(" Addresses: ", theme.faint())];
        let mut used = text::width(" Addresses: ");
        for (position, address) in completion.matches.iter().enumerate() {
            let more = completion.matches.len() - position;
            let tail = if more > 1 { 8 } else { 0 };
            if used + text::width(address) + 2 + tail > usize::from(width) && position > 0 {
                spans.push(Span::styled(format!("+{more} more"), theme.faint()));
                break;
            }
            let style = if completion.cycle == position {
                theme.reversed()
            } else {
                theme.bold()
            };
            spans.push(Span::styled(address.clone(), style));
            spans.push(Span::raw("  "));
            used += text::width(address) + 2;
        }
        frame
            .buffer_mut()
            .set_line(row.x, row.y, &Line::from(spans), row.width);
    }
}

/// The `^T` list over the body: addresses, or a folder's files.
fn draw_picker(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let Some(picker) = app
        .compose
        .as_ref()
        .and_then(|compose| compose.picker.clone())
    else {
        return;
    };
    clear(frame, body);
    let width = usize::from(body.width);
    let title = match &picker.kind {
        PickerKind::Addresses(field) => format!(
            "  ADDRESSES FOR {}: the ones you use most first",
            field.label().to_uppercase()
        ),
        PickerKind::Files { folder, read } => format!(
            "  {} {}",
            if *read { "INSERT FILE" } else { "ATTACH FILE" },
            zmail_security::terminal::sanitize_line(&folder.display().to_string())
        ),
    };
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit(&title, width, theme.ellipsis()),
            theme.bold().add_modifier(Modifier::UNDERLINED),
        )),
        PaneLine::default(),
    ];
    for (index, entry) in picker.entries.iter().enumerate() {
        let style = if index == picker.cursor {
            theme.reversed()
        } else if entry.folder {
            theme.bold()
        } else {
            Style::new()
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(&format!("    {}", entry.label), width, theme.ellipsis()),
                style,
            ))
        });
    }
    app.reveal.insert(PaneId::Picker);
    draw_pane(
        app,
        frame,
        PaneId::Picker,
        body,
        lines,
        Some(picker.cursor + 2),
    );
}

/// The postponed messages: when, to whom, the subject, and `@` for
/// attachments.
pub(super) fn postponed_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit(
                "  Messages you postponed. Enter continues one, D deletes it.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::default(),
    ];
    if app.postponed.is_empty() {
        lines.push(PaneLine::new(Span::raw(fit(
            "  No postponed messages.",
            width,
            theme.ellipsis(),
        ))));
        return lines;
    }
    let now = app.now();
    let to_width = (width / 4).clamp(8, 28);
    for (index, draft) in app.postponed.iter().enumerate() {
        let (to, subject) = App::postponed_label(draft);
        let date = format_list_date(&draft.updated_at, &now, app.hour_cycle);
        let row = format!(
            "  {} {}  {} {}",
            if draft.attachment_count > 0 { '@' } else { ' ' },
            fit_right(&date, 9, theme.ellipsis()),
            fit(&to, to_width, theme.ellipsis()),
            subject
        );
        let style = if index == app.postponed_cursor {
            theme.reversed()
        } else {
            Style::new()
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(fit(&row, width, theme.ellipsis()), style))
        });
    }
    lines
}

/// The line the Outbox's cursor is on: each message takes two lines.
pub(super) fn outbox_cursor_line(app: &App) -> usize {
    2 + app.outbox_cursor * 2
}

/// The Outbox: each message's status, then who it is to and its subject.
pub(super) fn outbox_lines(app: &App, theme: Theme, width: u16) -> Vec<PaneLine> {
    let width = usize::from(width);
    let mut lines = vec![
        PaneLine::new(Span::styled(
            fit(
                "  Messages not sent yet. R sends a failed one again, D takes one back.",
                width,
                theme.ellipsis(),
            ),
            theme.faint(),
        )),
        PaneLine::default(),
    ];
    if app.outbox.is_empty() {
        lines.push(PaneLine::new(Span::raw(fit(
            "  The Outbox is empty: everything has been sent.",
            width,
            theme.ellipsis(),
        ))));
        return lines;
    }
    for (index, row) in app.outbox.iter().enumerate() {
        let selected = index == app.outbox_cursor;
        let OutboxRow {
            to,
            subject,
            status,
            failed,
            error,
            ..
        } = row;
        let mut status_style = if *failed { theme.flag() } else { theme.bold() };
        let mut detail_style = Style::new();
        if selected {
            status_style = status_style.patch(theme.reversed());
            detail_style = detail_style.patch(theme.reversed());
        }
        // A refusal or a key change says what to do instead.
        let status_line = match error {
            Some(error) if row.can_retry => format!("  {status}: {error}"),
            _ => format!("  {status}"),
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(&status_line, width, theme.ellipsis()),
                status_style,
            ))
        });
        let to = if to.is_empty() { "(no recipients)" } else { to };
        let subject = if subject.is_empty() {
            "(no subject)"
        } else {
            subject
        };
        lines.push(PaneLine {
            target: Some(index),
            ..PaneLine::new(Span::styled(
                fit(&format!("    To {to}  {subject}"), width, theme.ellipsis()),
                detail_style,
            ))
        });
    }
    lines
}
