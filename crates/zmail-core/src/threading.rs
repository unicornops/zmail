//! A message's threading identity (RFC 5322 section 3.6.4) and the other
//! headers a reply needs: who else it went to (Cc) and who it was
//! delivered to.

/// The headers of a message that a reply to it is worked out from, as the
/// sync adapters read them.
///
/// Message ids are kept without their angle brackets, as mail parsers and
/// JMAP give them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageHeaders {
    /// Whether the adapter read these headers from the message itself.
    /// `false` for mail stored before Zmail kept them, and for a Proton
    /// message known only from its metadata (which has its Message-ID but
    /// not its In-Reply-To or References): the next fetch of the message
    /// fills them in.
    pub captured: bool,
    /// The message's `Message-ID`.
    pub message_id: Option<String>,
    /// The `In-Reply-To` ids: the message (or messages) it answers.
    pub in_reply_to: Vec<String>,
    /// The `References` ids, oldest first.
    pub references: Vec<String>,
    /// The `Cc` recipients, as a summary like the To recipients
    /// (`Name <address>, address`).
    pub cc: String,
    /// The addresses in `Delivered-To` (and `X-Original-To`): who the
    /// message was delivered to, which may be an alias the To and Cc
    /// headers do not name.
    pub delivered_to: Vec<String>,
}

/// How many ids a reply's References keeps: the conversation's first
/// message and the most recent ones. Long conversations would otherwise
/// grow the header without bound; RFC 5322 caps a line at 998 characters
/// and some servers refuse longer header fields.
pub const MAX_REFERENCES: usize = 20;

/// The reply action a draft came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyAction {
    /// A reply to the sender (or Reply-To).
    Reply,
    /// A reply to everyone on the message.
    ReplyAll,
    /// A forward.
    Forward,
}

impl ReplyAction {
    /// Stable storage value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::ReplyAll => "reply_all",
            Self::Forward => "forward",
        }
    }

    /// Parses a stored value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reply" => Some(Self::Reply),
            "reply_all" => Some(Self::ReplyAll),
            "forward" => Some(Self::Forward),
            _ => None,
        }
    }
}

/// The message a reply or forward draft answers, by where it is stored,
/// so sending can read its headers when the draft was made before they
/// were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftParent {
    /// Server name of the mailbox holding the message.
    pub mailbox: String,
    /// The message's remote id in that mailbox.
    pub remote_id: String,
}

/// What a draft answers: the action it came from and the headers that
/// tie it to the conversation (RFC 5322 section 3.6.4). Empty for a new
/// message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftThreading {
    /// The reply action the draft came from.
    pub action: Option<ReplyAction>,
    /// The `In-Reply-To` id: the answered message's Message-ID.
    pub in_reply_to: Option<String>,
    /// The `References` ids, oldest first.
    pub references: Vec<String>,
    /// The message answered.
    pub parent: Option<DraftParent>,
    /// The answered message's headers had not been read when the draft
    /// was made (mail stored before Zmail kept them): sending reads them
    /// and works the headers out again first.
    pub pending: bool,
}

impl DraftThreading {
    /// What a reply (or reply-all, with `action`) to a message with
    /// `parent_headers`, stored at `parent`, says. Forwards start a
    /// conversation of their own, so they carry no In-Reply-To or
    /// References.
    pub fn answering(
        action: ReplyAction,
        parent_headers: &MessageHeaders,
        parent: Option<DraftParent>,
    ) -> Self {
        let (in_reply_to, references) = match action {
            ReplyAction::Forward => (None, Vec::new()),
            ReplyAction::Reply | ReplyAction::ReplyAll => reply_ids(parent_headers),
        };
        Self {
            action: Some(action),
            in_reply_to,
            references,
            pending: action != ReplyAction::Forward && !parent_headers.captured && parent.is_some(),
            parent,
        }
    }
}

/// A reply's `In-Reply-To` and `References` (RFC 5322 section 3.6.4).
///
/// In-Reply-To is the parent's Message-ID. References is the parent's
/// References, or, when it has none, its In-Reply-To when that names a
/// single message, followed by the parent's Message-ID; a parent without
/// a Message-ID gives no In-Reply-To. Repeated ids are dropped, and
/// past [`MAX_REFERENCES`] the first id and the newest ones are kept.
pub fn reply_ids(parent: &MessageHeaders) -> (Option<String>, Vec<String>) {
    let message_id = parent.message_id.as_deref().and_then(bare_message_id);
    let mut references: Vec<String> = Vec::new();
    let inherited = if !parent.references.is_empty() {
        parent.references.as_slice()
    } else if parent.in_reply_to.len() == 1 {
        parent.in_reply_to.as_slice()
    } else {
        &[]
    };
    for id in inherited
        .iter()
        .filter_map(|id| bare_message_id(id))
        .chain(message_id.clone())
    {
        if !references.contains(&id) {
            references.push(id);
        }
    }
    if references.len() > MAX_REFERENCES {
        let newest = references.split_off(references.len() - (MAX_REFERENCES - 1));
        references.truncate(1);
        references.extend(newest);
    }
    (message_id, references)
}

/// A message id without surrounding white space or angle brackets, or
/// `None` when nothing is left.
pub fn bare_message_id(value: &str) -> Option<String> {
    let trimmed = value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim();
    (!trimmed.is_empty() && !trimmed.contains(char::is_whitespace)).then(|| trimmed.to_owned())
}

/// The message ids in a header value such as `<a@x> <b@y>`, in order,
/// without their angle brackets. Also takes ids already split by white
/// space or commas, with or without brackets.
pub fn parse_message_ids(value: &str) -> Vec<String> {
    let mut ids = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let Some(end) = rest[start..].find('>') else {
            break;
        };
        if let Some(id) = bare_message_id(&rest[start + 1..start + end]) {
            ids.push(id);
        }
        rest = &rest[start + end + 1..];
    }
    if ids.is_empty() {
        ids = value
            .split([' ', '\t', ','])
            .filter_map(bare_message_id)
            .collect();
    }
    ids
}

/// Message ids as a header value: each in angle brackets, separated by a
/// space.
pub fn format_message_ids<S: AsRef<str>>(ids: &[S]) -> String {
    ids.iter()
        .filter_map(|id| bare_message_id(id.as_ref()))
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        DraftParent, DraftThreading, MAX_REFERENCES, MessageHeaders, ReplyAction, bare_message_id,
        format_message_ids, parse_message_ids, reply_ids,
    };

    fn headers(
        message_id: Option<&str>,
        in_reply_to: &[&str],
        references: &[&str],
    ) -> MessageHeaders {
        MessageHeaders {
            captured: true,
            message_id: message_id.map(str::to_owned),
            in_reply_to: in_reply_to.iter().map(|id| (*id).to_owned()).collect(),
            references: references.iter().map(|id| (*id).to_owned()).collect(),
            ..MessageHeaders::default()
        }
    }

    #[test]
    fn a_reply_names_its_parent_and_extends_the_chain() {
        // The parent's References, then its Message-ID.
        assert_eq!(
            reply_ids(&headers(Some("c@x"), &["b@x"], &["a@x", "b@x"])),
            (
                Some("c@x".to_owned()),
                vec!["a@x".to_owned(), "b@x".to_owned(), "c@x".to_owned()]
            )
        );
        // No References: a single In-Reply-To stands in for them.
        assert_eq!(
            reply_ids(&headers(Some("b@x"), &["a@x"], &[])).1,
            ["a@x", "b@x"]
        );
        // Several In-Reply-To ids say nothing about the chain.
        assert_eq!(
            reply_ids(&headers(Some("c@x"), &["a@x", "b@x"], &[])).1,
            ["c@x"]
        );
        // A first message: just its id.
        assert_eq!(
            reply_ids(&headers(Some("a@x"), &[], &[])),
            (Some("a@x".to_owned()), vec!["a@x".to_owned()])
        );
        // No Message-ID: no In-Reply-To, but the chain it does name.
        assert_eq!(
            reply_ids(&headers(None, &[], &["a@x"])),
            (None, vec!["a@x".to_owned()])
        );
        // A parent that lists itself does not repeat.
        assert_eq!(
            reply_ids(&headers(Some("b@x"), &[], &["a@x", "b@x"])).1,
            ["a@x", "b@x"]
        );
    }

    #[test]
    fn a_long_chain_keeps_its_root_and_newest_ids() {
        let chain = (0..40).map(|n| format!("m{n}@x")).collect::<Vec<_>>();
        let chain_refs = chain.iter().map(String::as_str).collect::<Vec<_>>();
        let (in_reply_to, references) = reply_ids(&headers(Some("m40@x"), &[], &chain_refs));
        assert_eq!(in_reply_to.as_deref(), Some("m40@x"));
        assert_eq!(references.len(), MAX_REFERENCES);
        assert_eq!(references[0], "m0@x", "the conversation's root stays");
        assert_eq!(references[1], "m22@x");
        assert_eq!(references.last().map(String::as_str), Some("m40@x"));
    }

    #[test]
    fn a_draft_answers_a_reply_but_a_forward_starts_afresh() {
        let parent = || {
            Some(DraftParent {
                mailbox: "INBOX".to_owned(),
                remote_id: "7".to_owned(),
            })
        };
        let read = headers(Some("b@x"), &["a@x"], &["a@x"]);
        let reply = DraftThreading::answering(ReplyAction::ReplyAll, &read, parent());
        assert_eq!(reply.action, Some(ReplyAction::ReplyAll));
        assert_eq!(reply.in_reply_to.as_deref(), Some("b@x"));
        assert_eq!(reply.references, ["a@x", "b@x"]);
        assert!(!reply.pending);

        let forward = DraftThreading::answering(ReplyAction::Forward, &read, parent());
        assert_eq!(forward.in_reply_to, None);
        assert!(forward.references.is_empty() && !forward.pending);
        assert_eq!(forward.parent, parent());

        // Headers not read yet: sending reads them first.
        let unread = MessageHeaders {
            message_id: Some("b@x".to_owned()),
            ..MessageHeaders::default()
        };
        let reply = DraftThreading::answering(ReplyAction::Reply, &unread, parent());
        assert!(reply.pending);
        assert_eq!(reply.in_reply_to.as_deref(), Some("b@x"));
    }

    #[test]
    fn reply_actions_round_trip_through_storage() {
        for action in [
            ReplyAction::Reply,
            ReplyAction::ReplyAll,
            ReplyAction::Forward,
        ] {
            assert_eq!(ReplyAction::parse(action.as_str()), Some(action));
        }
        assert_eq!(ReplyAction::parse("other"), None);
    }

    #[test]
    fn message_ids_lose_their_brackets_and_keep_their_order() {
        assert_eq!(
            parse_message_ids("<a@example.com>\r\n <b@example.com> <c@x>"),
            ["a@example.com", "b@example.com", "c@x"]
        );
        assert_eq!(parse_message_ids("a@x b@y"), ["a@x", "b@y"]);
        assert!(parse_message_ids("  ").is_empty());
        assert_eq!(bare_message_id(" <id@host> ").as_deref(), Some("id@host"));
        assert_eq!(bare_message_id("<>"), None);
        assert_eq!(bare_message_id("two words"), None);
    }

    #[test]
    fn message_ids_format_as_a_header_value() {
        assert_eq!(format_message_ids(&["a@x", "<b@y>"]), "<a@x> <b@y>");
        assert_eq!(format_message_ids::<&str>(&[]), "");
    }
}
