//! Drawing the identity form on Change (`crate::identities`): a heading,
//! a labelled row per field with its problem under it, then the signature
//! in its editor below its label, as Compose draws the message text.
//!
//! The editors hold filtered text (`crate::identities` filters what it
//! loads and what is pasted); the heading's address is filtered here.

use ratatui::{Frame, layout::Rect, style::Style};
use zmail_security::terminal::sanitize_line;
use zmail_setup::identity_editor::{FormMode, IdentityField};

use super::Theme;
use crate::{app::App, text::fit};

/// Width of the labels, `Signature: `.
const LABEL_WIDTH: u16 = 11;

pub(super) fn draw_identity_form(app: &mut App, frame: &mut Frame, theme: Theme, body: Rect) {
    let Some(edit) = app.edit.as_mut() else {
        return;
    };
    let heading = match edit.form.as_ref().map(|form| form.mode) {
        Some(FormMode::Edit(identity_id)) => {
            let email = edit
                .identities
                .iter()
                .find(|identity| identity.identity.id == identity_id)
                .map(|identity| sanitize_line(&identity.identity.email_address))
                .unwrap_or_default();
            format!("Edit {email}")
        }
        _ => format!(
            "Add an identity to {}",
            sanitize_line(&edit.account.display_name)
        ),
    };
    let Some(form) = edit.form.as_mut() else {
        return;
    };
    let width = body.width;
    let full = usize::from(width);
    let bottom = body.y + body.height;
    let mut y = body.y;
    let buffer = frame.buffer_mut();
    buffer.set_stringn(
        body.x,
        y,
        fit(&format!("  {heading}"), full, theme.ellipsis()),
        full,
        theme.bold(),
    );
    y += 2;
    let field_width = width.saturating_sub(LABEL_WIDTH);
    for field in IdentityField::ALL {
        if y >= bottom {
            return;
        }
        let focused = form.focus == field;
        let label = format!("{:<9}: ", field.label());
        frame.buffer_mut().set_stringn(
            body.x,
            y,
            &label,
            full,
            if focused { theme.bold() } else { Style::new() },
        );
        if field == IdentityField::Signature {
            y += 1;
            // Room under it for the problem line, when there is one.
            let reserved = u16::from(form.error(field).is_some());
            let height = bottom.saturating_sub(y).saturating_sub(reserved);
            if height > 0 {
                let editor = form.editor_mut(field);
                editor.set_cursor_style(if focused {
                    theme.reversed()
                } else {
                    Style::new()
                });
                frame.render_widget(&*editor, Rect::new(body.x, y, width, height));
                y += height;
            }
        } else if field == IdentityField::Address && !form.address_editable {
            let fixed = form.text(field);
            let note = form.address_note.unwrap_or_default();
            let shown = format!("{fixed}  ({note})");
            frame.buffer_mut().set_stringn(
                body.x + LABEL_WIDTH,
                y,
                fit(&shown, usize::from(field_width), theme.ellipsis()),
                usize::from(field_width),
                theme.faint(),
            );
            y += 1;
        } else {
            let editor = form.editor_mut(field);
            editor.set_cursor_style(if focused {
                theme.reversed()
            } else {
                Style::new()
            });
            frame.render_widget(&*editor, Rect::new(body.x + LABEL_WIDTH, y, field_width, 1));
            y += 1;
        }
        if let Some(error) = form.error(field)
            && y < bottom
        {
            let error = format!("{}{error}", " ".repeat(usize::from(LABEL_WIDTH)));
            frame.buffer_mut().set_stringn(
                body.x,
                y,
                fit(&error, full, theme.ellipsis()),
                full,
                theme.flag(),
            );
            y += 1;
        }
    }
}
