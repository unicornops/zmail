//! Sender and participant display.
//!
//! Protocol adapters store a thread's participants as an RFC 5322 style
//! address list (`Alice Johnson <alice@example.com>, bob@example.com`):
//! the distinct senders of the thread's messages, oldest first. The shell
//! parses it back with [`parse_address_list`] to show display names, for
//! example "Alice, Bob (3)" via [`participant_summary`].
//!
//! Older mirrors stored `sender -> recipients` instead; the parser treats
//! ` -> ` as a list separator so those rows still read sensibly until the
//! next sync rewrites them.

use std::collections::HashSet;

use crate::MessageSnapshot;

/// One mailbox from an address header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    /// Display name, when the header carries one.
    pub name: Option<String>,
    /// Email address, when the header carries one.
    pub email: Option<String>,
}

impl Participant {
    /// Parses one mailbox such as `"Doe, Jane" <jane@example.com>`,
    /// `Jane <jane@example.com>`, `<jane@example.com>` or
    /// `jane@example.com`.
    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim();
        if let Some(start) = trimmed.rfind('<')
            && let Some(end) = trimmed.rfind('>')
            && start < end
        {
            let name = unquote(trimmed[..start].trim());
            let email = trimmed[start + 1..end].trim();
            return Self {
                name: (!name.is_empty()).then_some(name),
                email: (!email.is_empty()).then(|| email.to_owned()),
            };
        }
        if trimmed.contains('@') {
            Self {
                name: None,
                email: Some(trimmed.to_owned()),
            }
        } else {
            let name = unquote(trimmed);
            Self {
                name: (!name.is_empty()).then_some(name),
                email: None,
            }
        }
    }

    /// The name to show: the display name, else the address.
    pub fn display_name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.email.as_deref())
            .unwrap_or_default()
    }

    /// A short name for lists with several participants: the first word
    /// of the display name, else the address's local part.
    pub fn short_name(&self) -> &str {
        if let Some(name) = self.name.as_deref() {
            return name.split_whitespace().next().unwrap_or(name);
        }
        self.email
            .as_deref()
            .map(|email| email.split('@').next().unwrap_or(email))
            .unwrap_or_default()
    }

    /// The lowercased address used to compare participants.
    pub fn key(&self) -> String {
        self.email
            .as_deref()
            .or(self.name.as_deref())
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    }

    /// Formats the mailbox for an address list, quoting the display name
    /// when it contains characters that would otherwise split the list.
    pub fn to_header_value(&self) -> String {
        match (self.name.as_deref(), self.email.as_deref()) {
            (Some(name), Some(email)) => {
                if name.contains([',', ';', '<', '>', '"', '@', ':']) {
                    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
                    format!("\"{escaped}\" <{email}>")
                } else {
                    format!("{name} <{email}>")
                }
            }
            (None, Some(email)) => email.to_owned(),
            (Some(name), None) => name.to_owned(),
            (None, None) => String::new(),
        }
    }
}

/// Splits a sender string into its display name and address, for headers
/// that show both. When there is no display name, the address is used as
/// the name and no separate address is returned.
pub fn sender_identity(sender: &str) -> (String, Option<String>) {
    let participant = Participant::parse(sender);
    match (participant.name, participant.email) {
        (Some(name), email) => (name, email),
        (None, Some(email)) => (email, None),
        (None, None) => (sender.trim().to_owned(), None),
    }
}

/// The name to show for a sender string: its display name, else its
/// address.
pub fn sender_display_name(sender: &str) -> String {
    sender_identity(sender).0
}

/// Parses an address list, splitting on commas and semicolons outside
/// quotes and angle brackets.
///
/// An unquoted display name that itself contains a comma
/// (`Doe, Jane <jane@example.com>`) is joined back to the mailbox that
/// follows it. The legacy `sender -> recipients` form is accepted too.
pub fn parse_address_list(value: &str) -> Vec<Participant> {
    let normalized = value.replace(" -> ", ", ");
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut in_angle = false;
    let mut escaped = false;
    for ch in normalized.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => {
                current.push(ch);
                escaped = true;
            }
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            '<' if !in_quotes => {
                in_angle = true;
                current.push(ch);
            }
            '>' if !in_quotes => {
                in_angle = false;
                current.push(ch);
            }
            ',' | ';' if !in_quotes && !in_angle => {
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    parts.push(current);

    let mut participants = Vec::new();
    let mut pending_name: Option<String> = None;
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // A fragment with neither an address nor brackets is the first
        // half of an unquoted "Last, First <addr>" name.
        if !part.contains('@') && !part.contains('<') {
            pending_name = Some(match pending_name.take() {
                Some(previous) => format!("{previous}, {part}"),
                None => part.to_owned(),
            });
            continue;
        }
        let joined = match pending_name.take() {
            Some(prefix) if part.contains('<') => format!("{prefix}, {part}"),
            Some(prefix) => {
                participants.push(Participant::parse(&prefix));
                part.to_owned()
            }
            None => part.to_owned(),
        };
        participants.push(Participant::parse(&joined));
    }
    if let Some(name) = pending_name {
        participants.push(Participant::parse(&name));
    }
    participants
}

/// Formats participants as an address list that [`parse_address_list`]
/// reads back.
pub fn format_address_list(participants: &[Participant]) -> String {
    participants
        .iter()
        .map(Participant::to_header_value)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The participants of a thread: the distinct senders of its messages,
/// in the order they first wrote, as an address list.
pub fn thread_participants(messages: &[MessageSnapshot]) -> String {
    let mut ordered = messages.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.received_at.cmp(&right.received_at));
    let mut seen = HashSet::new();
    let participants = ordered
        .into_iter()
        .map(|message| Participant::parse(&message.sender))
        .filter(|participant| {
            let key = participant.key();
            !key.is_empty() && seen.insert(key)
        })
        .collect::<Vec<_>>();
    format_address_list(&participants)
}

/// A list-row summary of a thread's participants: `Alice Johnson` for one
/// sender, `Alice, Bob` for several, with the message count appended when
/// the thread has more than one message: `Alice, Bob (3)`.
///
/// Addresses in `own_addresses` show as "me". At most three names are
/// listed.
pub fn participant_summary(
    participants: &str,
    message_count: usize,
    own_addresses: &[&str],
) -> String {
    const MAX_NAMES: usize = 3;
    let own = own_addresses
        .iter()
        .map(|address| address.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let parsed = parse_address_list(participants);
    let mut seen = HashSet::new();
    let unique = parsed
        .iter()
        .filter(|participant| seen.insert(participant.key()))
        .collect::<Vec<_>>();
    let is_me = |participant: &Participant| {
        participant
            .email
            .as_deref()
            .is_some_and(|email| own.contains(&email.trim().to_ascii_lowercase()))
    };

    let names = if unique.len() == 1 {
        vec![if is_me(unique[0]) {
            "me"
        } else {
            unique[0].display_name()
        }]
    } else {
        unique
            .iter()
            .take(MAX_NAMES)
            .map(|participant| {
                if is_me(participant) {
                    "me"
                } else {
                    participant.short_name()
                }
            })
            .collect()
    };

    let mut summary = names.join(", ");
    if message_count > 1 {
        summary.push_str(&format!(" ({message_count})"));
    }
    summary
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let inner = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    inner.replace("\\\"", "\"").replace("\\\\", "\\")
}

#[cfg(test)]
mod tests {
    use super::{
        Participant, format_address_list, parse_address_list, participant_summary,
        sender_display_name, sender_identity, thread_participants,
    };
    use crate::MessageSnapshot;

    fn message(sender: &str, received_at: &str) -> MessageSnapshot {
        MessageSnapshot {
            remote_id: received_at.to_owned(),
            identity_email: "me@example.com".to_owned(),
            sender: sender.to_owned(),
            reply_to: None,
            recipients: "me@example.com".to_owned(),
            subject: "Subject".to_owned(),
            body_preview: String::new(),
            body_text: None,
            sanitized_html: None,
            has_remote_content: false,
            is_read: true,
            is_flagged: false,
            received_at: received_at.to_owned(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn sender_identity_splits_name_and_address() {
        assert_eq!(
            sender_identity("Alice Johnson <alice@example.com>"),
            (
                "Alice Johnson".to_owned(),
                Some("alice@example.com".to_owned())
            )
        );
        assert_eq!(
            sender_identity("\"Doe, Jane\" <jane@example.com>"),
            ("Doe, Jane".to_owned(), Some("jane@example.com".to_owned()))
        );
        assert_eq!(
            sender_identity("<bob@example.com>"),
            ("bob@example.com".to_owned(), None)
        );
        assert_eq!(
            sender_identity("bob@example.com"),
            ("bob@example.com".to_owned(), None)
        );
        assert_eq!(
            sender_identity("Unknown sender"),
            ("Unknown sender".to_owned(), None)
        );
        assert_eq!(
            sender_display_name("Grace Hopper <grace@example.com>"),
            "Grace Hopper"
        );
    }

    #[test]
    fn address_lists_round_trip_with_awkward_names() {
        let participants = vec![
            Participant::parse("Alice Johnson <alice@example.com>"),
            Participant {
                name: Some("Doe, Jane".to_owned()),
                email: Some("jane@example.com".to_owned()),
            },
            Participant {
                name: Some("Say \"Hi\"".to_owned()),
                email: Some("hi@example.com".to_owned()),
            },
            Participant::parse("bob@example.com"),
        ];
        let formatted = format_address_list(&participants);
        assert_eq!(
            formatted,
            "Alice Johnson <alice@example.com>, \"Doe, Jane\" <jane@example.com>, \"Say \\\"Hi\\\"\" <hi@example.com>, bob@example.com"
        );
        assert_eq!(parse_address_list(&formatted), participants);
    }

    #[test]
    fn unquoted_comma_names_are_rejoined() {
        let parsed = parse_address_list("Doe, Jane <jane@example.com>; bob@example.com");
        assert_eq!(
            parsed,
            vec![
                Participant {
                    name: Some("Doe, Jane".to_owned()),
                    email: Some("jane@example.com".to_owned()),
                },
                Participant::parse("bob@example.com"),
            ]
        );
        // Plain names without addresses (fixtures, older rows) survive.
        let names = parse_address_list("Kai Santos, Nora Walsh");
        assert_eq!(names.len(), 1, "unaddressed fragments are one name");
    }

    #[test]
    fn legacy_arrow_strings_parse_as_a_list() {
        let parsed =
            parse_address_list("Grace Hopper <grace@example.com> -> Rob <rob@example.com>");
        assert_eq!(
            parsed
                .iter()
                .map(Participant::display_name)
                .collect::<Vec<_>>(),
            vec!["Grace Hopper", "Rob"]
        );
    }

    #[test]
    fn thread_participants_are_distinct_senders_oldest_first() {
        let messages = vec![
            message("Bob Smith <bob@example.com>", "2026-10-15T10:00:00Z"),
            message("Alice Johnson <alice@example.com>", "2026-10-15T09:00:00Z"),
            message("ALICE Johnson <Alice@Example.com>", "2026-10-15T11:00:00Z"),
        ];
        assert_eq!(
            thread_participants(&messages),
            "Alice Johnson <alice@example.com>, Bob Smith <bob@example.com>"
        );
        assert_eq!(thread_participants(&[]), "");
    }

    #[test]
    fn summary_uses_full_name_alone_and_first_names_together() {
        assert_eq!(
            participant_summary("Alice Johnson <alice@example.com>", 1, &[]),
            "Alice Johnson"
        );
        assert_eq!(
            participant_summary("Alice Johnson <alice@example.com>", 4, &[]),
            "Alice Johnson (4)"
        );
        assert_eq!(
            participant_summary(
                "Alice Johnson <alice@example.com>, Bob Smith <bob@example.com>",
                3,
                &[]
            ),
            "Alice, Bob (3)"
        );
        assert_eq!(
            participant_summary("carol@example.com, Dan <dan@example.com>", 2, &[]),
            "carol, Dan (2)"
        );
    }

    #[test]
    fn summary_marks_own_addresses_and_caps_the_list() {
        assert_eq!(
            participant_summary(
                "Alice <alice@example.com>, Rob Example <rob@example.com>",
                2,
                &["ROB@example.com"]
            ),
            "Alice, me (2)"
        );
        assert_eq!(
            participant_summary(
                "A <a@x.test>, B <b@x.test>, C <c@x.test>, D <d@x.test>",
                5,
                &[]
            ),
            "A, B, C (5)"
        );
    }
}
