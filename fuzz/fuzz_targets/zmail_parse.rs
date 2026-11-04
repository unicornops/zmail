#![no_main]
//! Fuzzes zmail's mail parsing on hostile input.
//!
//! 1. zmail's own code: `zmail_imap::hydrated_body_from_rfc822`, the path
//!    every IMAP and Proton message body takes (mail-parser with the default
//!    settings, the body split, the HTML sanitiser, attachment extraction
//!    into a `BlobStore`, and the threading headers). It is called for real,
//!    so the target follows zmail when that code changes.
//! 2. What zmail-imap reads only while syncing a header fetch
//!    (`accumulate_fetch` and `conversation_key`, private and tied to an
//!    async-imap `Fetch`), mirrored here on mail-parser directly. Keep it in
//!    step with crates/zmail-imap/src/lib.rs.
//! 3. A wider sweep of mail-parser's view API over every part and nested
//!    message.
//!
//! mail-parser 1.0 builds many `&str` with `from_utf8_unchecked`; a bad span
//! would be undefined behaviour that AddressSanitizer does not see, so every
//! string handed out is validated again (`check`).

use libfuzzer_sys::fuzz_target;
use mail_parser::{AddressList, HeaderForm, Message, MessageParser, MessagePart, MessageRef};
use std::borrow::Cow;
use std::hint::black_box;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use zmail_core::{BlobStore, BlobWriter};

/// Keeps nothing: reads each blob and returns a name, as the store would.
#[derive(Debug)]
struct NullBlobs;

impl BlobWriter for NullBlobs {
    fn write_blob(&self, logical_name: &str, contents: &[u8]) -> std::io::Result<String> {
        check(logical_name);
        black_box(
            contents
                .iter()
                .fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        );
        Ok(logical_name.to_owned())
    }

    fn location(&self) -> &Path {
        Path::new("fuzz-null-blobs")
    }
}

static BLOBS: LazyLock<BlobStore> = LazyLock::new(|| BlobStore::new(Arc::new(NullBlobs)));

/// Aborts on a `&str` that is not valid UTF-8.
fn check(text: &str) {
    assert!(
        std::str::from_utf8(text.as_bytes()).is_ok(),
        "invalid UTF-8 in a &str: {:?}",
        text.as_bytes()
    );
    black_box(text.chars().count());
}

fn check_opt(text: Option<&str>) {
    if let Some(text) = text {
        check(text);
    }
}

/// zmail's real path.
fn hydrate(data: &[u8]) {
    let Some(body) = zmail_imap::hydrated_body_from_rfc822(data, "INBOX", "1", &BLOBS) else {
        return;
    };
    check_opt(body.body_text.as_deref());
    check_opt(body.sanitized_html.as_deref());
    check_opt(body.raw_html.as_deref());
    check(&body.body_preview);
    for attachment in &body.attachments {
        check(&attachment.filename);
        check(&attachment.content_type);
        check(&attachment.storage_path);
        check_opt(attachment.content_id.as_deref());
    }
    let headers = &body.headers;
    check_opt(headers.message_id.as_deref());
    headers.in_reply_to.iter().for_each(|id| check(id));
    headers.references.iter().for_each(|id| check(id));
    check(&headers.cc);
    headers
        .delivered_to
        .iter()
        .for_each(|address| check(address));
}

/// zmail-imap's `parse_message`: the default parser, and an empty message
/// for non-empty input mail-parser rejects.
fn parse_message<'x>(parser: &MessageParser, raw: &'x [u8]) -> Option<Message<'x>> {
    parser
        .parse(raw)
        .or_else(|| (!raw.is_empty()).then(|| parser.parse(b"\r\n")).flatten())
}

fn addresses(list: Option<AddressList<'_>>) -> Vec<String> {
    list.into_iter()
        .flat_map(|list| list.mailboxes())
        .map(|mailbox| {
            check_opt(mailbox.name());
            check_opt(mailbox.address());
            match (mailbox.name(), mailbox.address()) {
                (Some(name), Some(email)) if !name.trim().is_empty() => format!("{name} <{email}>"),
                (_, Some(email)) => email.to_owned(),
                (Some(name), None) => name.to_owned(),
                (None, None) => String::new(),
            }
        })
        .collect()
}

/// The reads of zmail-imap's `accumulate_fetch` and `conversation_key`
/// that `hydrated_body_from_rfc822` does not make.
macro_rules! sync_reads {
    ($message:expr) => {{
        let message = $message;
        check_opt(message.subject());
        black_box(addresses(message.from()));
        black_box(addresses(message.reply_to()));
        black_box(addresses(message.to()));
        black_box(message.date().map(|date| date.to_rfc3339()));
        // conversation_key
        let root = message
            .references()
            .and_then(|ids| ids.first().map(str::trim).filter(|id| !id.is_empty()))
            .or_else(|| {
                message
                    .in_reply_to()
                    .and_then(|ids| ids.first().map(str::trim).filter(|id| !id.is_empty()))
            })
            .or_else(|| {
                message
                    .message_id()
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
            });
        check_opt(root);
    }};
}

fn sweep_part(part: MessagePart<'_>) {
    black_box((part.kind(), part.flags(), part.role(), part.encoding()));
    black_box((
        part.raw().len(),
        part.raw_headers().len(),
        part.raw_body().len(),
    ));
    check_opt(part.attachment_name());
    check_opt(part.content_id());
    check_opt(part.content_description());
    check_opt(part.content_location());
    check_opt(part.content_transfer_encoding());
    check_opt(part.boundary());
    for content_type in [part.content_type(), part.content_disposition()]
        .into_iter()
        .flatten()
    {
        check(content_type.ctype());
        check_opt(content_type.subtype());
        for (name, value) in content_type.attributes() {
            check(name);
            check(value);
        }
    }
    let decoded = part.decoded();
    let (checked, problems) = part.decoded_checked();
    black_box((decoded.len(), checked.len(), problems, part.decoded_len()));
    if let Some(text) = part.text() {
        check(&text);
    }
    if let Some((text, _)) = part.text_checked() {
        check(&text);
    }
    for limit in [1, 80, 256] {
        if let Some(text) = part.text_prefix(limit) {
            check(&text);
        }
    }
    if let Some(html) = part.html_utf8() {
        check(&html);
    }
    for header in part.headers().iter() {
        check(header.raw_name());
        black_box(format!("{:?}", header.value()));
    }
}

fn sweep_message(message: MessageRef<'_>) {
    sync_reads!(message);
    check_opt(message.thread_name());
    check_opt(message.return_address());
    for ids in [
        message.in_reply_to(),
        message.references(),
        message.keywords(),
    ]
    .into_iter()
    .flatten()
    {
        ids.iter().for_each(check);
    }
    for received in message.all_received() {
        black_box(format!("{received:?}"));
    }
    for text in message.text_bodies().chain(message.html_bodies()) {
        check(&text);
    }
    for index in 0..2 {
        message.body_text(index).as_deref().map(check);
        message.body_html(index).as_deref().map(check);
    }
    if let Some(preview) = message.body_preview(180) {
        check(&preview);
    }
    for header in message.headers().iter() {
        for form in [
            HeaderForm::Text,
            HeaderForm::Addresses,
            HeaderForm::MessageIds,
            HeaderForm::Date,
            HeaderForm::ContentType,
            HeaderForm::Received,
        ] {
            black_box(format!("{:?}", header.parse_as(form)));
        }
    }
}

fuzz_target!(|data: &[u8]| {
    hydrate(data);

    let parser = MessageParser::default();
    let Some(message) = parse_message(&parser, data) else {
        return;
    };
    sync_reads!(&message);
    for nested in message.messages() {
        sweep_message(nested);
    }
    for part in message.parts() {
        sweep_part(part);
    }
    let owned = message.into_owned();
    black_box(owned.body_text(0).map(Cow::into_owned));
});
