//! Replies and forwards: the recipients, subject and body a new draft
//! starts with, worked out the same way in both frontends.
//!
//! - **Reply** goes to the message's sender; **reply all** adds its other
//!   recipients as Cc. The user's own addresses are left out, and when
//!   the sender is the user (a reply to their own message), the first
//!   remaining recipient moves to To.
//! - The subject gets `Re:` or `Fwd:`, once.
//! - A reply quotes the message under an "On …, … wrote:" line; a forward
//!   copies it under a "Forwarded message" block with its headers.
//!
//! Nothing here reads the store or knows a UI toolkit: the frontend
//! passes the message, the user's addresses and the message's date
//! formatted for its own time zone, and creates the draft with
//! [`MailActions::create_draft`](crate::MailActions::create_draft).
//! Neither frontend sets `In-Reply-To` or `References` yet: a draft has no
//! field for them.

use std::collections::HashSet;

use anyhow::{Result, bail};
use zmail_core::{IdentitySummary, ThreadMessage};

/// What a reply or forward draft starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftStart {
    /// To recipients.
    pub to: Vec<String>,
    /// Cc recipients.
    pub cc: Vec<String>,
    /// The subject, with its prefix.
    pub subject: String,
    /// The body: two empty lines to write in, then the quoted or
    /// forwarded message.
    pub body: String,
}

/// The user's own addresses, normalised, for leaving them out of a reply.
pub fn own_addresses(identities: &[IdentitySummary]) -> HashSet<String> {
    identities
        .iter()
        .map(|identity| normalize_contact(&identity.email_address))
        .filter(|value| !value.is_empty())
        .collect()
}

/// A reply to `message` in a conversation called `thread_subject`, to all
/// its recipients when `reply_all` is set. `date` is when the message
/// arrived, as the attribution line shows it.
///
/// Fails when nobody is left to reply to (the message has no sender and no
/// recipient other than the user).
pub fn reply(
    thread_subject: &str,
    message: &ThreadMessage,
    own_addresses: &HashSet<String>,
    reply_all: bool,
    date: &str,
) -> Result<DraftStart> {
    let (to, cc) = reply_recipients(message, own_addresses, reply_all)?;
    Ok(DraftStart {
        to,
        cc,
        subject: prefixed_subject("Re", thread_subject),
        body: quoted_reply_body(message, date),
    })
}

/// A forward of `message` in a conversation called `thread_subject`, with
/// no recipients yet. `date` is when the message arrived, as the forwarded
/// headers show it.
pub fn forward(thread_subject: &str, message: &ThreadMessage, date: &str) -> DraftStart {
    DraftStart {
        to: Vec::new(),
        cc: Vec::new(),
        subject: prefixed_subject("Fwd", thread_subject),
        body: forwarded_body(message, date),
    }
}

/// Who a reply to `message` goes to: the sender in To and, for reply all,
/// the other recipients in Cc, without the user's own addresses or
/// duplicates.
pub fn reply_recipients(
    message: &ThreadMessage,
    own_addresses: &HashSet<String>,
    reply_all: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let sender = message.sender.trim().to_owned();
    let sender_key = normalize_contact(&sender);

    let mut to =
        if sender.is_empty() || sender_key.is_empty() || own_addresses.contains(&sender_key) {
            Vec::new()
        } else {
            vec![sender]
        };

    let mut cc = if reply_all {
        dedupe_contacts(
            split_contact_summary(&message.recipients)
                .into_iter()
                .filter(|recipient| {
                    let normalized = normalize_contact(recipient);
                    !normalized.is_empty()
                        && normalized != sender_key
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
    use std::collections::HashSet;

    use zmail_core::{IdentitySummary, MessageRenderMode, ThreadMessage};

    use super::{
        forward, normalize_contact, own_addresses, prefixed_subject, reply, split_contact_summary,
    };

    fn message(sender: &str, recipients: &str) -> ThreadMessage {
        ThreadMessage {
            id: 1,
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
        }
    }

    fn me() -> HashSet<String> {
        own_addresses(&[IdentitySummary {
            id: 1,
            display_name: "Me".to_owned(),
            email_address: "Me@Work.example".to_owned(),
            signature: String::new(),
            is_primary: true,
        }])
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
        let start = reply("Quarterly report", &message, &me(), true, "today").expect("recipients");
        assert_eq!(start.to, ["Kai Santos <kai@example.com>"]);
        assert_eq!(start.cc, ["Ada <ada@example.com>"]);
    }

    #[test]
    fn a_reply_to_the_users_own_message_goes_to_its_recipients() {
        let message = message(
            "Me <me@work.example>",
            "Ada <ada@example.com>, bo@example.com",
        );
        let start = reply("Plan", &message, &me(), true, "today").expect("recipients");
        assert_eq!(start.to, ["Ada <ada@example.com>"]);
        assert_eq!(start.cc, ["bo@example.com"]);
        assert!(
            reply("Plan", &message, &me(), false, "today").is_err(),
            "a plain reply to oneself has nobody to go to"
        );
    }

    #[test]
    fn a_forward_copies_the_message_with_its_headers() {
        let message = message("Kai Santos <kai@example.com>", "");
        let start = forward("Quarterly report", &message, "Sat 21 Mar 2026");
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
