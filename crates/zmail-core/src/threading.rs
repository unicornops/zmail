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
    use super::{bare_message_id, format_message_ids, parse_message_ids};

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
