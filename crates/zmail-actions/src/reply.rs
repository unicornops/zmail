//! Replies and forwards: the recipients, identity, subject, body and
//! attachments a new draft starts with, worked out the same way in both
//! frontends.
//!
//! - **Reply** goes to the message's `Reply-To` when it has one, else to
//!   its sender; **reply all** adds its other To and Cc recipients as Cc.
//!   The user's own addresses are left out.
//! - A reply to the user's **own message** (its sender is one of the
//!   account's identities) goes where that message went, as Thunderbird
//!   and Apple Mail do: reply to its To, reply all to its To and Cc. A
//!   message the user sent only to themselves is answered to themselves.
//! - The draft is sent from the identity the message was addressed to (in
//!   To, Cc or Delivered-To, or the one that sent it), else the primary
//!   ([`reply_identity`]); the user can change it in compose.
//! - The subject gets `Re:` or `Fwd:`, once.
//! - A reply quotes the message under an "On …, … wrote:" line; a forward
//!   copies it under a "Forwarded message" block with its headers, and
//!   takes its attachments along.
//! - A reply names the message it answers in `In-Reply-To` and extends
//!   its `References` (RFC 5322 section 3.6.4,
//!   [`DraftThreading::answering`]); a forward starts a conversation of its
//!   own. Both record the message they came from, so sending can fill in
//!   headers that had not been read yet.
//!
//! Nothing here reads the store or knows a UI toolkit: the frontend
//! passes the message, the mailbox it is in, the account's identities and
//! the message's date formatted for its own time zone, and creates the
//! draft with
//! [`MailActions::create_started_draft`](crate::MailActions::create_started_draft).

use std::collections::HashSet;

use anyhow::{Result, bail};
use zmail_core::{
    DraftParent, DraftThreading, IdentitySummary, ReceivedAttachment, ReplyAction, ThreadMessage,
};

/// What a reply or forward draft starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStart {
    /// The identity it is sent from ([`reply_identity`]); `None` when the
    /// account has none.
    pub identity_id: Option<i64>,
    /// To recipients.
    pub to: Vec<String>,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// The subject, with its prefix.
    pub subject: String,
    /// The body: two empty lines to write in, then the quoted or
    /// forwarded message.
    pub body: String,
    /// What the draft answers.
    pub threading: DraftThreading,
    /// The received attachments a forward takes along: the ones the reader
    /// lists. Parts kept only for an HTML body's `cid:` images are not
    /// among them; the forward carries the message's text, not its HTML.
    pub attachments: Vec<ReceivedAttachment>,
}

/// The user's own addresses, normalised, for leaving them out of a reply.
pub fn own_addresses(identities: &[IdentitySummary]) -> HashSet<String> {
    identities
        .iter()
        .map(|identity| normalize_contact(&identity.email_address))
        .filter(|value| !value.is_empty())
        .collect()
}

/// A reply to `message`, in mailbox `mailbox`, in a conversation called
/// `thread_subject`, to all its recipients when `reply_all` is set, from
/// one of the account's `identities`. `date` is when the message arrived,
/// as the attribution line shows it.
///
/// Fails when nobody is left to reply to (the message has no sender and no
/// recipient other than the user). A message from the user always has
/// someone: themselves, at worst.
pub fn reply(
    thread_subject: &str,
    mailbox: &str,
    message: &ThreadMessage,
    identities: &[IdentitySummary],
    reply_all: bool,
    date: &str,
) -> Result<DraftStart> {
    let (to, cc) = reply_recipients(message, &own_addresses(identities), reply_all)?;
    let action = if reply_all {
        ReplyAction::ReplyAll
    } else {
        ReplyAction::Reply
    };
    Ok(DraftStart {
        identity_id: reply_identity(message, identities).map(|identity| identity.id),
        to,
        cc,
        subject: prefixed_subject("Re", thread_subject),
        body: quoted_reply_body(message, date),
        threading: threading(action, mailbox, message),
        attachments: Vec::new(),
    })
}

/// A forward of `message`, in mailbox `mailbox`, in a conversation called
/// `thread_subject`, with no recipients yet, from one of the account's
/// `identities`. `date` is when the message arrived, as the forwarded
/// headers show it.
pub fn forward(
    thread_subject: &str,
    mailbox: &str,
    message: &ThreadMessage,
    identities: &[IdentitySummary],
    date: &str,
) -> DraftStart {
    DraftStart {
        identity_id: reply_identity(message, identities).map(|identity| identity.id),
        to: Vec::new(),
        cc: Vec::new(),
        subject: prefixed_subject("Fwd", thread_subject),
        body: forwarded_body(message, date),
        threading: threading(ReplyAction::Forward, mailbox, message),
        attachments: message.attachments.clone(),
    }
}

/// What a draft made by `action` from `message` answers.
fn threading(action: ReplyAction, mailbox: &str, message: &ThreadMessage) -> DraftThreading {
    let parent = (!message.remote_id.is_empty() && !mailbox.is_empty()).then(|| DraftParent {
        mailbox: mailbox.to_owned(),
        remote_id: message.remote_id.clone(),
    });
    DraftThreading::answering(action, &message.headers, parent)
}

/// The identity a reply to (or forward of) `message` is sent from: the
/// one that sent it (a reply from Sent), else the first the message was
/// addressed to in To, then Cc, then Delivered-To (an alias or a Bcc), an
/// address with a `+tag` matching the identity without it; else the
/// primary identity, else the first.
pub fn reply_identity<'a>(
    message: &ThreadMessage,
    identities: &'a [IdentitySummary],
) -> Option<&'a IdentitySummary> {
    let addressed = [message.sender.as_str()]
        .into_iter()
        .map(normalize_contact)
        .chain(
            split_contact_summary(&message.recipients)
                .iter()
                .chain(&split_contact_summary(&message.headers.cc))
                .chain(&message.headers.delivered_to)
                .map(|address| normalize_contact(address)),
        )
        .filter(|address| !address.is_empty())
        .collect::<Vec<_>>();
    let identity_of = |address: &str| {
        identities
            .iter()
            .find(|identity| normalize_contact(&identity.email_address) == address)
    };
    addressed
        .iter()
        .find_map(|address| identity_of(address))
        .or_else(|| {
            addressed
                .iter()
                .find_map(|address| identity_of(&without_subaddress(address)))
        })
        .or_else(|| identities.iter().find(|identity| identity.is_primary))
        .or_else(|| identities.first())
}

/// `local+tag@domain` as `local@domain`.
fn without_subaddress(address: &str) -> String {
    match address.split_once('@') {
        Some((local, domain)) => match local.split_once('+') {
            Some((base, _)) if !base.is_empty() => format!("{base}@{domain}"),
            _ => address.to_owned(),
        },
        None => address.to_owned(),
    }
}

/// Who a reply to `message` goes to: its `Reply-To` (else its sender) in
/// To and, for reply all, its other To and Cc recipients in Cc, without
/// the user's own addresses or duplicates. With a `Reply-To`, the sender
/// is left out, as they asked. A message the user sent is answered to its
/// recipients ([`own_message_recipients`]).
pub fn reply_recipients(
    message: &ThreadMessage,
    own_addresses: &HashSet<String>,
    reply_all: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let sender = message.sender.trim().to_owned();
    let sender_key = normalize_contact(&sender);
    if !sender_key.is_empty() && own_addresses.contains(&sender_key) {
        return Ok(own_message_recipients(
            message,
            &sender,
            own_addresses,
            reply_all,
        ));
    }
    let reply_to = dedupe_contacts(
        message
            .reply_to
            .as_deref()
            .map(split_contact_summary)
            .unwrap_or_default()
            .into_iter()
            .filter(|address| {
                let normalized = normalize_contact(address);
                !normalized.is_empty() && !own_addresses.contains(&normalized)
            })
            .collect(),
    );

    let mut to = if sender_key.is_empty() {
        Vec::new()
    } else if !reply_to.is_empty() {
        reply_to
    } else {
        vec![sender]
    };

    let mut cc = if reply_all {
        let in_to = to
            .iter()
            .map(|address| normalize_contact(address))
            .collect::<HashSet<_>>();
        dedupe_contacts(
            split_contact_summary(&message.recipients)
                .into_iter()
                .chain(split_contact_summary(&message.headers.cc))
                .filter(|recipient| {
                    let normalized = normalize_contact(recipient);
                    !normalized.is_empty()
                        && normalized != sender_key
                        && !in_to.contains(&normalized)
                        && !own_addresses.contains(&normalized)
                })
                .collect(),
        )
    } else {
        Vec::new()
    };

    if to.is_empty() && !cc.is_empty() {
        to.push(cc.remove(0));
    }

    if to.is_empty() {
        bail!("the selected message does not expose reply recipients");
    }
    Ok((to, cc))
}

/// Who a reply to the user's own `message`, sent from `sender`, goes to:
/// its To recipients again and, for reply all, its Cc recipients in Cc,
/// without the user's addresses or duplicates. Reply all on a message
/// whose To held only the user moves the first Cc recipient to To; one
/// sent to nobody else goes back to `sender`.
fn own_message_recipients(
    message: &ThreadMessage,
    sender: &str,
    own_addresses: &HashSet<String>,
    reply_all: bool,
) -> (Vec<String>, Vec<String>) {
    let others = |summary: &str| {
        split_contact_summary(summary)
            .into_iter()
            .filter(|address| {
                let normalized = normalize_contact(address);
                !normalized.is_empty() && !own_addresses.contains(&normalized)
            })
            .collect::<Vec<_>>()
    };
    let mut to = dedupe_contacts(others(&message.recipients));
    let mut cc = if reply_all {
        let in_to = to
            .iter()
            .map(|address| normalize_contact(address))
            .collect::<HashSet<_>>();
        dedupe_contacts(
            others(&message.headers.cc)
                .into_iter()
                .filter(|address| !in_to.contains(&normalize_contact(address)))
                .collect(),
        )
    } else {
        Vec::new()
    };
    if to.is_empty() {
        if cc.is_empty() {
            return (vec![sender.to_owned()], Vec::new());
        }
        to.push(cc.remove(0));
    }
    (to, cc)
}

/// `subject` with `prefix:` in front, unless it already starts with it.
pub fn prefixed_subject(prefix: &str, subject: &str) -> String {
    let trimmed = subject.trim();
    let normalized_prefix = format!("{}:", prefix.to_ascii_lowercase());

    if trimmed.to_ascii_lowercase().starts_with(&normalized_prefix) {
        trimmed.to_owned()
    } else if trimmed.is_empty() {
        format!("{prefix}:")
    } else {
        format!("{prefix}: {trimmed}")
    }
}

/// A reply's body: room to write, the attribution line, and the message
/// quoted with `>`.
pub fn quoted_reply_body(message: &ThreadMessage, date: &str) -> String {
    format!(
        "\n\nOn {}, {} wrote:\n{}",
        date,
        message.sender,
        quote_text(&message.rendered_body)
    )
}

/// A forward's body: room to write, then the message with its headers.
pub fn forwarded_body(message: &ThreadMessage, date: &str) -> String {
    format!(
        "\n\n---------- Forwarded message ----------\nFrom: {}\nDate: {}\nTo: {}\nSubject: {}\n\n{}",
        message.sender,
        date,
        if message.recipients.trim().is_empty() {
            "(unknown)"
        } else {
            message.recipients.trim()
        },
        message.subject,
        message.rendered_body
    )
}

/// Each line of `body` behind `> ` (an empty line behind `>`).
pub fn quote_text(body: &str) -> String {
    body.lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A recipient summary (`a@example.com, b@example.com; c@example.com`)
/// split into its addresses.
pub fn split_contact_summary(value: &str) -> Vec<String> {
    value
        .split([',', ';'])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `values` without a second entry for the same address.
pub fn dedupe_contacts(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::new();

    for value in values {
        let normalized = normalize_contact(&value);
        if normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        deduped.push(value);
    }

    deduped
}

/// The lowercased address in `value` (`Alice <alice@example.com>` gives
/// `alice@example.com`), for comparing recipients.
pub fn normalize_contact(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if let Some(start) = trimmed.rfind('<')
        && let Some(end) = trimmed.rfind('>')
        && start < end
    {
        return trimmed[start + 1..end].trim().to_ascii_lowercase();
    }

    trimmed.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use zmail_core::{
        DraftParent, IdentitySummary, MessageHeaders, MessageRenderMode, ReceivedAttachment,
        ReplyAction, ThreadMessage,
    };

    use super::{
        forward, normalize_contact, own_addresses, prefixed_subject, reply, reply_identity,
        reply_recipients, split_contact_summary,
    };

    fn message(sender: &str, recipients: &str) -> ThreadMessage {
        ThreadMessage {
            id: 1,
            remote_id: "1".to_owned(),
            sender: sender.to_owned(),
            reply_to: None,
            recipients: recipients.to_owned(),
            subject: "Quarterly report".to_owned(),
            body_preview: String::new(),
            body_text: None,
            sanitized_html: None,
            rendered_body: "Numbers attached.\n\nKai".to_owned(),
            body_links: Vec::new(),
            render_mode: MessageRenderMode::PlainText,
            is_read: true,
            is_flagged: false,
            has_remote_content: false,
            remote_content_sender_allowed: false,
            sender_warning_dismissed: false,
            received_at: "2026-03-21T09:15:00Z".to_owned(),
            attachments: Vec::new(),
            headers: zmail_core::MessageHeaders {
                captured: true,
                ..Default::default()
            },
        }
    }

    fn identity(id: i64, email: &str, is_primary: bool) -> IdentitySummary {
        IdentitySummary {
            id,
            display_name: "Me".to_owned(),
            email_address: email.to_owned(),
            signature: String::new(),
            is_primary,
        }
    }

    fn me() -> Vec<IdentitySummary> {
        vec![identity(1, "Me@Work.example", true)]
    }

    #[test]
    fn prefixed_subject_avoids_duplicate_reply_prefix() {
        assert_eq!(prefixed_subject("Re", "Re: Existing"), "Re: Existing");
        assert_eq!(prefixed_subject("Re", "RE: Existing"), "RE: Existing");
        assert_eq!(prefixed_subject("Fwd", "Subject"), "Fwd: Subject");
        assert_eq!(prefixed_subject("Re", "  "), "Re:");
    }

    #[test]
    fn normalize_contact_extracts_bracketed_email() {
        assert_eq!(
            normalize_contact("Alice Example <alice@example.com>"),
            "alice@example.com"
        );
        assert_eq!(normalize_contact("bob@example.com"), "bob@example.com");
    }

    #[test]
    fn split_contact_summary_supports_commas_and_semicolons() {
        assert_eq!(
            split_contact_summary("alice@example.com, bob@example.com; carol@example.com"),
            vec!["alice@example.com", "bob@example.com", "carol@example.com",]
        );
    }

    #[test]
    fn a_reply_goes_to_the_sender_and_quotes_the_message() {
        let message = message(
            "Kai Santos <kai@example.com>",
            "me@work.example, Ada <ada@example.com>",
        );
        let start = reply(
            "Quarterly report",
            "INBOX",
            &message,
            &me(),
            false,
            "Sat 21 Mar 2026",
        )
        .expect("someone to reply to");
        assert_eq!(start.to, ["Kai Santos <kai@example.com>"]);
        assert!(start.cc.is_empty());
        assert_eq!(start.subject, "Re: Quarterly report");
        assert_eq!(
            start.body,
            "\n\nOn Sat 21 Mar 2026, Kai Santos <kai@example.com> wrote:\n\
             > Numbers attached.\n>\n> Kai"
        );
    }

    #[test]
    fn reply_all_adds_the_other_recipients_without_the_user_or_duplicates() {
        let message = message(
            "Kai Santos <kai@example.com>",
            "ME@work.example, Ada <ada@example.com>; ada@example.com, kai@example.com",
        );
        let start =
            reply("Quarterly report", "INBOX", &message, &me(), true, "today").expect("recipients");
        assert_eq!(start.to, ["Kai Santos <kai@example.com>"]);
        assert_eq!(start.cc, ["Ada <ada@example.com>"]);
    }

    #[test]
    fn a_reply_to_the_users_own_message_goes_to_its_recipients() {
        let mut message = message(
            "Me <me@work.example>",
            "Ada <ada@example.com>, bo@example.com, ME@work.example, ada@example.com",
        );
        message.headers.cc = "Cy <cy@example.com>, bo@example.com, me@work.example".to_owned();
        // A Reply-To on one's own message does not send the reply there.
        message.reply_to = Some("me@work.example".to_owned());

        let start = reply("Plan", "Sent", &message, &me(), false, "today").expect("recipients");
        assert_eq!(
            start.to,
            ["Ada <ada@example.com>", "bo@example.com"],
            "the original To, without the user or duplicates"
        );
        assert!(start.cc.is_empty(), "a plain reply leaves Cc out");
        assert_eq!(
            start.identity_id,
            Some(1),
            "sent from the identity that sent it"
        );
        assert_eq!(start.threading.action, Some(ReplyAction::Reply));

        let all = reply("Plan", "Sent", &message, &me(), true, "today").expect("recipients");
        assert_eq!(all.to, ["Ada <ada@example.com>", "bo@example.com"]);
        assert_eq!(
            all.cc,
            ["Cy <cy@example.com>"],
            "the original Cc, without the user or anyone already in To"
        );
    }

    #[test]
    fn a_message_the_user_sent_only_to_themselves_is_answered_to_them() {
        let mut note = message("Me <me@work.example>", "me@work.example");
        for reply_all in [false, true] {
            let start =
                reply("Note", "Sent", &note, &me(), reply_all, "today").expect("recipients");
            assert_eq!(start.to, ["Me <me@work.example>"]);
            assert!(start.cc.is_empty());
        }

        // Nobody else in To: a plain reply goes to the user, reply all to
        // the Cc recipients, the first of them in To.
        note.headers.cc = "Ada <ada@example.com>, bo@example.com".to_owned();
        let (to, cc) = reply_recipients(&note, &own_addresses(&me()), false).expect("to");
        assert_eq!(
            (to, cc),
            (vec!["Me <me@work.example>".to_owned()], Vec::new())
        );
        let (to, cc) = reply_recipients(&note, &own_addresses(&me()), true).expect("to");
        assert_eq!(to, ["Ada <ada@example.com>"]);
        assert_eq!(cc, ["bo@example.com"]);

        // From another of the user's identities: still their own message.
        let identities = vec![
            identity(1, "me@work.example", true),
            identity(2, "alias@work.example", false),
        ];
        let sent = message("alias@work.example", "Kai <kai@example.com>");
        let start = reply("Plan", "Sent", &sent, &identities, false, "today").expect("to");
        assert_eq!(start.to, ["Kai <kai@example.com>"]);
        assert_eq!(start.identity_id, Some(2));
    }

    #[test]
    fn a_reply_answers_its_message_and_a_forward_records_it() {
        let mut message = message("Kai Santos <kai@example.com>", "me@work.example");
        message.headers = MessageHeaders {
            captured: true,
            message_id: Some("m2@example.com".to_owned()),
            in_reply_to: vec!["m1@example.com".to_owned()],
            references: vec!["m1@example.com".to_owned()],
            ..MessageHeaders::default()
        };
        let parent = Some(DraftParent {
            mailbox: "INBOX".to_owned(),
            remote_id: "1".to_owned(),
        });

        let start = reply("Plan", "INBOX", &message, &me(), false, "today").expect("a reply");
        assert_eq!(start.threading.action, Some(ReplyAction::Reply));
        assert_eq!(
            start.threading.in_reply_to.as_deref(),
            Some("m2@example.com")
        );
        assert_eq!(
            start.threading.references,
            ["m1@example.com", "m2@example.com"]
        );
        assert_eq!(start.threading.parent, parent);
        assert!(!start.threading.pending);

        let all = reply("Plan", "INBOX", &message, &me(), true, "today").expect("a reply");
        assert_eq!(all.threading.action, Some(ReplyAction::ReplyAll));

        let forward = forward("Plan", "INBOX", &message, &me(), "today");
        assert_eq!(forward.threading.action, Some(ReplyAction::Forward));
        assert_eq!(forward.threading.in_reply_to, None);
        assert!(forward.threading.references.is_empty());
        assert_eq!(forward.threading.parent, parent);

        // Mail stored before Zmail kept its headers: sending reads them.
        message.headers = MessageHeaders::default();
        let start = reply("Plan", "INBOX", &message, &me(), false, "today").expect("a reply");
        assert!(start.threading.pending);
        assert_eq!(start.threading.in_reply_to, None);
    }

    #[test]
    fn a_reply_goes_to_reply_to_and_reply_all_adds_to_and_cc() {
        let mut message = message(
            "Kai Santos <kai@example.com>",
            "Me <me@work.example>, Ada <ada@example.com>",
        );
        message.reply_to = Some("Team <team@lists.example>, kai@example.com".to_owned());
        message.headers.cc = "Bo <bo@example.com>, ME@work.example, ada@example.com".to_owned();

        let (to, cc) = reply_recipients(&message, &own_addresses(&me()), false).expect("to");
        assert_eq!(to, ["Team <team@lists.example>", "kai@example.com"]);
        assert!(cc.is_empty());

        let (to, cc) = reply_recipients(&message, &own_addresses(&me()), true).expect("to");
        assert_eq!(to, ["Team <team@lists.example>", "kai@example.com"]);
        assert_eq!(
            cc,
            ["Ada <ada@example.com>", "Bo <bo@example.com>"],
            "To and Cc, without the user, duplicates or anyone already in To"
        );

        // Without Reply-To: the sender, and Cc joins reply-all.
        message.reply_to = None;
        let (to, cc) = reply_recipients(&message, &own_addresses(&me()), true).expect("to");
        assert_eq!(to, ["Kai Santos <kai@example.com>"]);
        assert_eq!(cc, ["Ada <ada@example.com>", "Bo <bo@example.com>"]);

        // A Reply-To naming only the user falls back to the sender.
        message.reply_to = Some("me@work.example".to_owned());
        let (to, _) = reply_recipients(&message, &own_addresses(&me()), false).expect("to");
        assert_eq!(to, ["Kai Santos <kai@example.com>"]);
    }

    #[test]
    fn a_reply_comes_from_the_identity_the_message_was_addressed_to() {
        let identities = vec![
            identity(1, "me@work.example", true),
            identity(2, "Alias@Work.example", false),
            identity(3, "lists@home.example", false),
        ];
        let id = |message: &ThreadMessage| {
            reply_identity(message, &identities).map(|identity| identity.id)
        };

        // To names the alias (any case).
        let mut to_alias = message(
            "kai@example.com",
            "Ada <ada@example.com>, alias@work.example",
        );
        assert_eq!(id(&to_alias), Some(2));
        let start = reply("Plan", "INBOX", &to_alias, &identities, false, "today").expect("reply");
        assert_eq!(start.identity_id, Some(2));
        assert_eq!(
            forward("Plan", "INBOX", &to_alias, &identities, "today").identity_id,
            Some(2)
        );

        // Cc, then Delivered-To (a Bcc or a list), then a +tag.
        to_alias.recipients = "ada@example.com".to_owned();
        to_alias.headers.cc = "Lists <lists@home.example>".to_owned();
        assert_eq!(id(&to_alias), Some(3));
        to_alias.headers.cc = String::new();
        to_alias.headers.delivered_to = vec!["ALIAS@work.example".to_owned()];
        assert_eq!(id(&to_alias), Some(2));
        to_alias.headers.delivered_to = vec!["lists+rust@home.example".to_owned()];
        assert_eq!(id(&to_alias), Some(3));

        // A message the user sent from the alias: that identity.
        let sent = message("Me <alias@work.example>", "kai@example.com");
        assert_eq!(id(&sent), Some(2));

        // Addressed to none of them: the primary.
        let elsewhere = message("kai@example.com", "list@example.com");
        assert_eq!(id(&elsewhere), Some(1));
        assert_eq!(reply_identity(&elsewhere, &[]), None);
    }

    #[test]
    fn a_forward_takes_the_attachments_along_and_a_reply_does_not() {
        let mut message = message("Kai Santos <kai@example.com>", "me@work.example");
        message.attachments = vec![ReceivedAttachment {
            id: 7,
            filename: "report.pdf".to_owned(),
            content_type: "application/pdf".to_owned(),
            size_bytes: 1024,
            storage_path: "blob".to_owned(),
        }];
        let forward = forward("Plan", "INBOX", &message, &me(), "today");
        assert_eq!(forward.attachments, message.attachments);
        let reply = reply("Plan", "INBOX", &message, &me(), false, "today").expect("reply");
        assert!(reply.attachments.is_empty());
    }

    #[test]
    fn a_forward_copies_the_message_with_its_headers() {
        let message = message("Kai Santos <kai@example.com>", "");
        let start = forward(
            "Quarterly report",
            "INBOX",
            &message,
            &me(),
            "Sat 21 Mar 2026",
        );
        assert!(start.to.is_empty() && start.cc.is_empty());
        assert_eq!(start.subject, "Fwd: Quarterly report");
        assert_eq!(
            start.body,
            "\n\n---------- Forwarded message ----------\nFrom: Kai Santos <kai@example.com>\n\
             Date: Sat 21 Mar 2026\nTo: (unknown)\nSubject: Quarterly report\n\n\
             Numbers attached.\n\nKai"
        );
    }
}
