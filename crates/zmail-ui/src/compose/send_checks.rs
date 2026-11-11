//! What Send checks before it queues a draft.
//!
//! - Problems that stop the send: no recipients at all, or a recipient
//!   that is not a valid address ([`Recipient::is_valid`]). The compose
//!   view says what to fix in a banner and focuses the field at fault.
//! - Checks the user can wave through with Send Anyway ([`SendCheck`]): an
//!   empty subject, and a message that mentions an attachment with nothing
//!   attached. Each asks once per press of Send.
//!
//! [`Recipient::is_valid`]: super::recipients::Recipient::is_valid

/// What Send says when the draft has no recipients.
pub(crate) const NO_RECIPIENTS: &str = "Add at least one recipient in To before sending.";

/// What Send says while attachments are still being copied in.
pub(crate) const WAIT_FOR_ATTACHMENTS: &str =
    "Wait for the attachments to finish copying, then send.";

/// What Send says when the draft is already waiting in the Outbox.
pub(crate) const ALREADY_QUEUED: &str = "This message is already in the Outbox, waiting to send.";

/// What Send says when the draft has gone already.
pub(crate) const ALREADY_SENT: &str = "This message has already been sent.";

/// The words that suggest the writer meant to attach something. English
/// only, and kept short so the question stays rare.
const ATTACHMENT_WORDS: [&str; 4] = ["attached", "attachment", "attachments", "enclosed"];

/// The line a forward's copy of the original message starts under
/// (`zmail_actions::reply::forward`): its text is not the writer's.
const FORWARDED_MESSAGE: &str = "---------- Forwarded message";

/// A check Send asks about before it queues the draft, which the user can
/// wave through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SendCheck {
    /// The subject is empty.
    NoSubject,
    /// The message mentions an attachment, but nothing is attached.
    NoAttachment,
}

impl SendCheck {
    /// The confirmation's title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::NoSubject => "Send without a subject?",
            Self::NoAttachment => "Send without an attachment?",
        }
    }

    /// The confirmation's explanation.
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::NoSubject => {
                "This message has no subject, which makes it harder for its recipients to \
                 spot and find again."
            }
            Self::NoAttachment => {
                "Your message mentions an attachment, but nothing is attached. Choose Cancel, \
                 then Attach to add it."
            }
        }
    }
}

/// What Send says about recipients that are not valid addresses, each a
/// field label ("To") and the recipient as typed.
pub(crate) fn invalid_recipients_message(invalid: &[(String, String)]) -> String {
    let named = invalid
        .iter()
        .map(|(field, recipient)| format!("“{recipient}” in {field}"))
        .collect::<Vec<_>>();
    match named.as_slice() {
        [] => String::new(),
        [one] => {
            format!("{one} isn't a valid email address. Correct it or remove it, then send again.")
        }
        [rest @ .., last] => format!(
            "These aren't valid email addresses: {} and {last}. Correct or remove them, \
             then send again.",
            rest.join(", ")
        ),
    }
}

/// Whether the writer's own text mentions an attachment: one of
/// [`ATTACHMENT_WORDS`], as a whole word in any case, outside quoted lines
/// (`>`) and a forwarded message.
pub(crate) fn mentions_attachment(body: &str) -> bool {
    body.lines()
        .take_while(|line| !line.trim_start().starts_with(FORWARDED_MESSAGE))
        .filter(|line| !line.trim_start().starts_with('>'))
        .flat_map(|line| line.split(|ch: char| !ch.is_alphanumeric()))
        .any(|word| {
            ATTACHMENT_WORDS
                .iter()
                .any(|attachment_word| word.eq_ignore_ascii_case(attachment_word))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_words_count_only_as_whole_words_in_the_writers_text() {
        assert!(mentions_attachment("Please see the attached report."));
        assert!(mentions_attachment("ATTACHMENT: minutes"));
        assert!(mentions_attachment("Two attachments follow"));
        assert!(mentions_attachment("I have enclosed the invoice."));
        assert!(!mentions_attachment("Detached house, unattached garage"));
        assert!(!mentions_attachment("Nothing to see here."));
        // Quoted text and a forwarded message are someone else's words.
        assert!(!mentions_attachment(
            "Thanks!\n\nOn Mon, Kai wrote:\n> The report is attached."
        ));
        assert!(!mentions_attachment(
            "FYI\n\n---------- Forwarded message ----------\nFrom: Kai\n\nSee attached."
        ));
    }

    #[test]
    fn invalid_recipients_are_named_with_their_field() {
        assert_eq!(
            invalid_recipients_message(&[("Cc".to_owned(), "design team".to_owned())]),
            "“design team” in Cc isn't a valid email address. Correct it or remove it, then \
             send again."
        );
        assert_eq!(
            invalid_recipients_message(&[
                ("To".to_owned(), "a@b..org".to_owned()),
                ("To".to_owned(), "c@".to_owned()),
                ("Cc".to_owned(), "design team".to_owned()),
            ]),
            "These aren't valid email addresses: “a@b..org” in To, “c@” in To and “design \
             team” in Cc. Correct or remove them, then send again."
        );
    }
}
