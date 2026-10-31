//! A conversation as the Message Text screen shows it: each message's
//! headers, attachments and text (the Simple view, from
//! [`zmail_render::prepare`]), with its links as numbered references.
//!
//! Everything from the mail goes through the terminal filter here, before
//! anything is drawn: subjects, senders, recipients, file names, bodies,
//! link text and addresses.

use std::ops::Range;

use anyhow::{Context, Result};
use chrono::FixedOffset;
use zmail_core::{
    ThreadMessage,
    time::{HourCycle, format_full_date},
};
use zmail_render::{PrepareOptions, prepare, render_source};
use zmail_security::terminal::{sanitize, sanitize_line};
use zmail_store::LocalStore;

use crate::links::{link_target, written_urls};

/// A line of a conversation, before wrapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocLine {
    /// `label: value`, one header.
    Header {
        /// `From`, `To`, …
        label: &'static str,
        /// The filtered value.
        value: String,
    },
    /// A line of text, with the byte ranges of the link references in it
    /// and the link each one opens.
    Text {
        /// The filtered text.
        text: String,
        /// `[n]` references: their byte range in `text`, and the index in
        /// [`Conversation::links`].
        links: Vec<(Range<usize>, usize)>,
    },
    /// A separator with a title ("Message 2 of 3", "Links").
    Rule(String),
    /// Something Zmail says about the message, not from it.
    Note(String),
    /// An empty line.
    Blank,
}

/// A link of the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocLink {
    /// The address that opens, normalised and checked by
    /// [`link_target`].
    pub url: String,
    /// The link's text in the message, filtered.
    pub text: String,
}

/// A conversation ready to wrap and draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// The thread shown.
    pub thread_id: i64,
    /// The account it belongs to.
    pub account_id: i64,
    /// Its subject, filtered.
    pub subject: String,
    /// How many messages it had when read.
    pub message_count: usize,
    /// The lines, in order.
    pub lines: Vec<DocLine>,
    /// Its links, numbered from 1 in the text.
    pub links: Vec<DocLink>,
    /// The first line of each message, by message id.
    pub message_starts: Vec<(i64, usize)>,
    /// Some message bodies haven't been downloaded yet.
    pub missing_bodies: bool,
}

/// Reads thread `thread_id` from the store; `None` when it's gone.
pub fn load(
    store: &LocalStore,
    thread_id: i64,
    account_id: i64,
    zone: &FixedOffset,
    hour_cycle: HourCycle,
) -> Result<Option<Conversation>> {
    let Some(view) = store.thread_view(thread_id)? else {
        return Ok(None);
    };
    let missing_bodies = store
        .messages_missing_bodies(thread_id)?
        .is_some_and(|missing| !missing.is_empty());
    let mut conversation = Conversation {
        thread_id,
        account_id,
        subject: sanitize_line(&view.subject),
        message_count: view.messages.len(),
        lines: Vec::new(),
        links: Vec::new(),
        message_starts: Vec::new(),
        missing_bodies,
    };
    let count = view.messages.len();
    for (index, message) in view.messages.iter().enumerate() {
        if count > 1 {
            conversation
                .lines
                .push(DocLine::Rule(format!("Message {} of {count}", index + 1)));
        }
        conversation
            .message_starts
            .push((message.id, conversation.lines.len()));
        add_message(store, &mut conversation, message, zone, hour_cycle)?;
        conversation.lines.push(DocLine::Blank);
    }
    if !conversation.links.is_empty() {
        conversation.lines.push(DocLine::Rule("Links".to_owned()));
        let lines = conversation
            .links
            .iter()
            .enumerate()
            .map(|(index, link)| {
                let marker = format!("[{}]", index + 1);
                DocLine::Text {
                    links: vec![(0..marker.len(), index)],
                    text: format!("{marker} {}", sanitize_line(&link.url)),
                }
            })
            .collect::<Vec<_>>();
        conversation.lines.extend(lines);
    }
    Ok(Some(conversation))
}

fn add_message(
    store: &LocalStore,
    conversation: &mut Conversation,
    message: &ThreadMessage,
    zone: &FixedOffset,
    hour_cycle: HourCycle,
) -> Result<()> {
    let lines = &mut conversation.lines;
    let header = |label, value: &str| DocLine::Header {
        label,
        value: sanitize_line(value),
    };
    lines.push(header(
        "Date",
        &format_full_date(&message.received_at, zone, hour_cycle),
    ));
    lines.push(header("From", &message.sender));
    if let Some(reply_to) = message
        .reply_to
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        lines.push(header("Reply-To", reply_to));
    }
    if !message.recipients.trim().is_empty() {
        lines.push(header("To", &message.recipients));
    }
    lines.push(header("Subject", &message.subject));
    for attachment in &message.attachments {
        lines.push(DocLine::Header {
            label: "Attachment",
            value: format!(
                "{} ({})",
                sanitize_line(&attachment.filename),
                size_label(attachment.size_bytes)
            ),
        });
    }
    lines.push(DocLine::Blank);

    let source = render_source(store, message.id)
        .with_context(|| format!("failed to read message {}", message.id))?;
    let prepared = source.map(|source| prepare(&source, &PrepareOptions::default()));
    // The body's text and its links: the HTML's links with their targets,
    // or the addresses written in plain text.
    let (text, links) = match &prepared {
        Some(prepared) if !prepared.text.trim().is_empty() => {
            let links = if prepared.text_is_plain {
                written_urls(&prepared.text)
                    .into_iter()
                    .filter_map(|range| {
                        let url = link_target(&prepared.text[range.clone()])?;
                        Some((range, url))
                    })
                    .collect()
            } else {
                prepared
                    .text_links
                    .iter()
                    .filter_map(|link| {
                        let url = link_target(&prepared.link(link.link)?.url)?;
                        Some((link.range.clone(), url))
                    })
                    .collect()
            };
            (prepared.text.as_str(), links)
        }
        _ => (message.body_preview.as_str(), Vec::new()),
    };
    let body_missing = prepared
        .as_ref()
        .is_none_or(|prepared| prepared.text.trim().is_empty());
    add_body(conversation, text, links);
    if body_missing && conversation.missing_bodies {
        conversation.lines.push(DocLine::Note(
            "The rest of this message hasn't been downloaded yet.".to_owned(),
        ));
    }
    Ok(())
}

/// Adds `text` as lines, with a `[n]` reference after each link's text.
fn add_body(conversation: &mut Conversation, text: &str, links: Vec<(Range<usize>, String)>) {
    let mut out = String::with_capacity(text.len());
    let mut markers = Vec::new();
    let mut cursor = 0;
    let mut links = links;
    links.sort_by_key(|(range, _)| range.start);
    for (range, url) in links {
        // Overlapping or broken ranges keep their text and lose the link.
        if range.start < cursor
            || range.end > text.len()
            || range.start >= range.end
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            continue;
        }
        out.push_str(&sanitize(&text[cursor..range.start]));
        out.push_str(&sanitize(&text[range.clone()]));
        let index = conversation.links.len();
        conversation.links.push(DocLink {
            url,
            text: sanitize_line(&text[range.clone()]),
        });
        let marker = format!("[{}]", index + 1);
        markers.push((out.len()..out.len() + marker.len(), index));
        out.push_str(&marker);
        cursor = range.end;
    }
    out.push_str(&sanitize(&text[cursor..]));

    let mut line_start = 0;
    let mut markers = markers.into_iter().peekable();
    for line in out.trim_end().split('\n') {
        let line_end = line_start + line.len();
        let mut line_links = Vec::new();
        while let Some((range, index)) =
            markers.next_if(|(range, _)| range.start >= line_start && range.end <= line_end)
        {
            line_links.push((range.start - line_start..range.end - line_start, index));
        }
        conversation.lines.push(DocLine::Text {
            text: line.to_owned(),
            links: line_links,
        });
        line_start = line_end + 1;
    }
}

/// `12 KB`, `3.4 MB`.
pub fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    match bytes {
        bytes if bytes < KB => format!("{bytes} B"),
        bytes if bytes < MB => format!("{} KB", bytes.div_ceil(KB)),
        bytes => format!("{:.1} MB", bytes as f64 / MB as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::{Conversation, DocLine, add_body, size_label};

    fn empty() -> Conversation {
        Conversation {
            thread_id: 1,
            account_id: 1,
            subject: String::new(),
            message_count: 1,
            lines: Vec::new(),
            links: Vec::new(),
            message_starts: Vec::new(),
            missing_bodies: false,
        }
    }

    #[test]
    fn links_get_numbered_references_and_text_is_filtered() {
        let mut conversation = empty();
        let text = "Hi \x1b[31mthere\x1b[0m,\nsee the site\nand https://b.example now";
        let site = text.find("the site").expect("link text");
        let written = text.find("https://b.example").expect("address");
        add_body(
            &mut conversation,
            text,
            vec![
                (written..written + 17, "https://b.example/".to_owned()),
                (site..site + 8, "https://a.example/".to_owned()),
            ],
        );
        assert_eq!(
            conversation.lines,
            vec![
                DocLine::Text {
                    text: "Hi there,".to_owned(),
                    links: Vec::new()
                },
                DocLine::Text {
                    text: "see the site[1]".to_owned(),
                    links: vec![(12..15, 0)]
                },
                DocLine::Text {
                    text: "and https://b.example[2] now".to_owned(),
                    links: vec![(21..24, 1)]
                },
            ]
        );
        assert_eq!(conversation.links[0].url, "https://a.example/");
        assert_eq!(conversation.links[0].text, "the site");
        assert_eq!(conversation.links[1].url, "https://b.example/");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(size_label(10), "10 B");
        assert_eq!(size_label(12 * 1024), "12 KB");
        assert_eq!(size_label(3 * 1024 * 1024 + 400_000), "3.4 MB");
    }
}
