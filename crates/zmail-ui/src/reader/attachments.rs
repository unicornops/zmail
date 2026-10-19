//! Attachment cards: one per received attachment, below the message body.
//!
//! A card shows a type icon (chosen by extension, then MIME type), the
//! file name and a readable size, with Open and Save As buttons. The
//! buttons dispatch [`OpenAttachment`] and [`SaveAttachmentAs`]; the
//! workspace saves the file and warns before opening a program or script
//! (see `crate::attachments`).

use gpui::{AnyElement, FontWeight, IntoElement, SharedString, div, prelude::*, px, rgb};
use zmail_core::{ReceivedAttachment, ThreadMessage};
use zmail_security::attachment_safety::is_executable_or_script;

use crate::actions::{OpenAttachment, SaveAttachmentAs};
use crate::components::{ButtonSize, IconButton};
use crate::styles::{
    ICON_SIZE_MD, RADIUS_MD, SPACE_0_5, SPACE_2, SPACE_3, TYPE_BODY, TYPE_CAPTION, TypeStyleExt,
    UiColors,
};
use crate::tooltip::text_tooltip;
use crate::widgets::{EllipsisExt, format_size, icon};

/// Width of an attachment card; cards wrap onto more rows in a narrow
/// pane.
const CARD_WIDTH: f32 = 300.0;

/// Side of the square behind a card's type icon.
const TYPE_ICON_FRAME: f32 = 36.0;

/// One card per attachment of `message`.
pub(crate) fn attachment_cards(message: &ThreadMessage, colors: &UiColors) -> Vec<AnyElement> {
    message
        .attachments
        .iter()
        .map(|attachment| attachment_card(attachment, colors).into_any_element())
        .collect()
}

fn attachment_card(attachment: &ReceivedAttachment, colors: &UiColors) -> impl IntoElement {
    let attachment_id = attachment.id;
    let kind = AttachmentKind::of(&attachment.filename, &attachment.content_type);
    let name = SharedString::from(attachment.filename.clone());
    let detail = format!(
        "{} \u{00B7} {}",
        kind.label,
        format_size(attachment.size_bytes)
    );
    let icon_color = if kind.executable {
        colors.warning
    } else {
        colors.muted
    };

    div()
        .id(("attachment-card", attachment_id as u64))
        .debug_selector(|| format!("attachment-{attachment_id}"))
        .flex()
        .items_center()
        .gap(px(SPACE_3))
        .w(px(CARD_WIDTH))
        .max_w_full()
        .min_w_0()
        .px(px(SPACE_2))
        .py(px(SPACE_2))
        .rounded(px(RADIUS_MD))
        .bg(rgb(colors.background))
        .border_1()
        .border_color(rgb(colors.border))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(TYPE_ICON_FRAME))
                .rounded(px(RADIUS_MD))
                .bg(rgb(colors.button_background))
                .child(icon(kind.icon, ICON_SIZE_MD, icon_color)),
        )
        .child(
            div()
                .id(("attachment-name", attachment_id as u64))
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(SPACE_0_5))
                .tooltip(text_tooltip(name.to_string(), *colors))
                .child(
                    div()
                        .w_full()
                        .ellipsis()
                        .type_style(TYPE_BODY)
                        .font_weight(FontWeight::MEDIUM)
                        .child(name.clone()),
                )
                .child(
                    div()
                        .w_full()
                        .ellipsis()
                        .type_style(TYPE_CAPTION)
                        .text_color(rgb(colors.muted))
                        .child(detail),
                ),
        )
        .child(
            div()
                .debug_selector(|| format!("attachment-open-{attachment_id}"))
                .child(
                    IconButton::new(
                        ("attachment-open", attachment_id as u64),
                        "external-link",
                        "Open",
                    )
                    .size(ButtonSize::Small)
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(OpenAttachment { attachment_id }), cx);
                    }),
                ),
        )
        .child(
            div()
                .debug_selector(|| format!("attachment-save-{attachment_id}"))
                .child(
                    IconButton::new(
                        ("attachment-save", attachment_id as u64),
                        "download",
                        "Save As…",
                    )
                    .size(ButtonSize::Small)
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(SaveAttachmentAs { attachment_id }), cx);
                    }),
                ),
        )
}

/// What a card says an attachment is: its icon and a short type label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AttachmentKind {
    pub(crate) icon: &'static str,
    pub(crate) label: &'static str,
    /// A program or script; the icon is drawn in the warning colour.
    pub(crate) executable: bool,
}

impl AttachmentKind {
    /// The kind of `filename` with MIME type `content_type`. The extension
    /// wins over the MIME type, which senders often leave generic.
    pub(crate) fn of(filename: &str, content_type: &str) -> Self {
        let executable = is_executable_or_script(filename, content_type);
        let extension = filename
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let (icon, label) = if executable {
            ("file-terminal", "Program")
        } else {
            by_extension(&extension)
                .or_else(|| by_mime(&mime))
                .unwrap_or(("file", "File"))
        };
        Self {
            icon,
            label,
            executable,
        }
    }
}

fn by_extension(extension: &str) -> Option<(&'static str, &'static str)> {
    Some(match extension {
        "pdf" => ("file-text", "PDF"),
        "txt" | "md" | "rtf" => ("file-text", "Text"),
        "doc" | "docx" | "odt" | "pages" => ("file-text", "Document"),
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => ("file-spreadsheet", "Spreadsheet"),
        "ppt" | "pptx" | "odp" | "key" => ("presentation", "Presentation"),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tif" | "tiff"
        | "svg" => ("file-image", "Image"),
        "mp4" | "mov" | "m4v" | "webm" | "mkv" | "avi" => ("file-video-camera", "Video"),
        "mp3" | "m4a" | "aac" | "wav" | "flac" | "ogg" | "opus" => ("file-music", "Audio"),
        "zip" | "gz" | "tgz" | "tar" | "bz2" | "xz" | "7z" | "rar" | "zst" => {
            ("file-archive", "Archive")
        }
        "ics" | "ical" | "ifb" => ("calendar", "Calendar"),
        "json" | "xml" | "html" | "htm" | "css" | "yaml" | "yml" | "toml" | "diff" | "patch" => {
            ("file-code", "Code")
        }
        "asc" | "pgp" | "gpg" | "sig" | "p7s" | "p7m" | "pem" | "crt" | "cer" => {
            ("file-key", "Signature")
        }
        _ => return None,
    })
}

fn by_mime(mime: &str) -> Option<(&'static str, &'static str)> {
    let (top, sub) = mime.split_once('/')?;
    Some(match (top, sub) {
        ("image", _) => ("file-image", "Image"),
        ("video", _) => ("file-video-camera", "Video"),
        ("audio", _) => ("file-music", "Audio"),
        ("text", "calendar") => ("calendar", "Calendar"),
        ("text", "html" | "xml" | "css") => ("file-code", "Code"),
        ("text", _) => ("file-text", "Text"),
        ("application", "pdf") => ("file-text", "PDF"),
        ("application", "zip" | "gzip" | "x-tar" | "x-7z-compressed" | "vnd.rar") => {
            ("file-archive", "Archive")
        }
        ("application", "pgp-signature" | "pgp-keys" | "pkcs7-signature") => {
            ("file-key", "Signature")
        }
        ("application", "json" | "xml") => ("file-code", "Code"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_the_extension_then_the_mime_type() {
        let kind = |name, mime| AttachmentKind::of(name, mime);
        assert_eq!(
            kind("report.pdf", "application/octet-stream").icon,
            "file-text"
        );
        assert_eq!(kind("report.PDF", "").label, "PDF");
        assert_eq!(kind("photo.jpeg", "image/jpeg").icon, "file-image");
        assert_eq!(kind("scan", "image/png").icon, "file-image");
        assert_eq!(kind("budget.xlsx", "").icon, "file-spreadsheet");
        assert_eq!(kind("deck.pptx", "").icon, "presentation");
        assert_eq!(kind("icons.zip", "application/zip").icon, "file-archive");
        assert_eq!(kind("invite.ics", "text/calendar").icon, "calendar");
        assert_eq!(kind("clip.mov", "").icon, "file-video-camera");
        assert_eq!(kind("memo.m4a", "").icon, "file-music");
        assert_eq!(kind("signature.asc", "").icon, "file-key");
        assert_eq!(
            kind("data.bin.unknown", "application/octet-stream").icon,
            "file"
        );
    }

    #[test]
    fn programs_and_scripts_are_marked() {
        let kind = AttachmentKind::of("setup.exe", "application/octet-stream");
        assert!(kind.executable);
        assert_eq!(kind.icon, "file-terminal");
        let kind = AttachmentKind::of("invoice.pdf.js", "application/pdf");
        assert!(kind.executable);
        assert!(!AttachmentKind::of("invoice.pdf", "application/pdf").executable);
    }

    #[test]
    fn every_card_icon_is_bundled() {
        let icons = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons");
        let extensions = [
            "pdf", "txt", "docx", "xlsx", "pptx", "png", "mp4", "mp3", "zip", "ics", "json", "asc",
            "exe", "unknown",
        ];
        let used = extensions
            .iter()
            .map(|extension| AttachmentKind::of(&format!("a.{extension}"), "").icon)
            .chain(["external-link", "download"]);
        for name in used {
            assert!(
                icons.join(format!("{name}.svg")).is_file(),
                "assets/icons/{name}.svg is missing"
            );
        }
    }
}
