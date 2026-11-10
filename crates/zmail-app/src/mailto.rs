//! `mailto:` links (RFC 6068), which come from web pages and other apps:
//! untrusted. A link whose addresses hold control characters once decoded
//! is refused whole.

/// A mailto URL decomposed into draft fields.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MailtoDraft {
    pub(crate) to: Vec<String>,
    pub(crate) cc: Vec<String>,
    pub(crate) bcc: Vec<String>,
    pub(crate) subject: String,
    pub(crate) body: String,
}

/// Parses an RFC 6068 mailto URL into draft fields, or `None` when it is
/// not one or an address in it holds a control character.
pub(crate) fn parse_mailto(raw: &str) -> Option<MailtoDraft> {
    let rest = raw.strip_prefix("mailto:")?;
    let (recipients, query) = match rest.split_once('?') {
        Some((recipients, query)) => (recipients, Some(query)),
        None => (rest, None),
    };

    let mut draft = MailtoDraft {
        to: split_addresses(recipients)?,
        cc: Vec::new(),
        bcc: Vec::new(),
        subject: String::new(),
        body: String::new(),
    };

    let Some(query) = query else {
        return Some(draft);
    };
    for pair in query.split('&') {
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        match percent_decode(name).to_ascii_lowercase().as_str() {
            "to" => draft.to.extend(split_addresses(value)?),
            "cc" => draft.cc = split_addresses(value)?,
            "bcc" => draft.bcc = split_addresses(value)?,
            // A subject is one line.
            "subject" => {
                draft.subject = percent_decode(value)
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect()
            }
            "body" => draft.body = percent_decode(value),
            _ => {}
        }
    }
    Some(draft)
}

/// Splits a comma-separated recipient list, then decodes each address,
/// dropping empty entries. `None` when an address holds a control
/// character (such as an encoded CR or LF).
fn split_addresses(raw: &str) -> Option<Vec<String>> {
    let mut addresses = Vec::new();
    for part in raw.split(',') {
        let address = percent_decode(part);
        if address.chars().any(char::is_control) {
            return None;
        }
        let address = address.trim();
        if !address.is_empty() {
            addresses.push(address.to_owned());
        }
    }
    Some(addresses)
}

/// Percent-decodes a mailto component. `+` stays a `+` (RFC 6068 has no
/// form encoding), and a `%` not followed by two hex digits stays as it
/// is.
fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (
                bytes.get(index + 1).copied().and_then(hex_value),
                bytes.get(index + 2).copied().and_then(hex_value),
            )
        {
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mailto_extracts_recipients_and_headers() {
        let draft = parse_mailto("mailto:person@example.com").expect("plain recipient parses");
        assert_eq!(draft.to, vec!["person@example.com".to_owned()]);
        assert_eq!(draft.subject, "");

        let draft = parse_mailto(
            "mailto:one@example.com,two@example.com?subject=Hello%20there&body=Hi%20again",
        )
        .expect("recipients with headers parse");
        assert_eq!(
            draft.to,
            vec!["one@example.com".to_owned(), "two@example.com".to_owned()]
        );
        assert_eq!(draft.subject, "Hello there");
        assert_eq!(draft.body, "Hi again");

        let draft = parse_mailto(
            "mailto:team@example.com?cc=lead@example.com&bcc=audit@example.com&subject=Sprint",
        )
        .expect("cc and bcc parse");
        assert_eq!(draft.cc, vec!["lead@example.com".to_owned()]);
        assert_eq!(draft.bcc, vec!["audit@example.com".to_owned()]);

        assert!(
            parse_mailto("https://example.com").is_none(),
            "non-mailto URLs are rejected"
        );
    }

    #[test]
    fn plus_is_a_plus() {
        let draft = parse_mailto("mailto:?to=user+tag@example.com&subject=1+1&body=a+b")
            .expect("it parses");
        assert_eq!(draft.to, vec!["user+tag@example.com".to_owned()]);
        assert_eq!(draft.subject, "1+1");
        assert_eq!(draft.body, "a+b");
    }

    #[test]
    fn path_recipients_are_decoded() {
        let draft =
            parse_mailto("mailto:ada%40example.com,%22Bo%20B%22%40example.com").expect("parses");
        assert_eq!(
            draft.to,
            vec![
                "ada@example.com".to_owned(),
                "\"Bo B\"@example.com".to_owned()
            ]
        );
        // An encoded comma is part of the address, not a separator.
        let draft = parse_mailto("mailto:%22a%2Cb%22@example.com").expect("parses");
        assert_eq!(draft.to, vec!["\"a,b\"@example.com".to_owned()]);
    }

    #[test]
    fn percent_escapes_at_the_end_and_malformed_ones() {
        assert_eq!(percent_decode("a%41"), "aA");
        assert_eq!(percent_decode("%41"), "A");
        assert_eq!(percent_decode("50%"), "50%");
        assert_eq!(percent_decode("50%4"), "50%4");
        assert_eq!(percent_decode("%zz"), "%zz");
        // `u8::from_str_radix` would take a sign: "+1" is not hex here.
        assert_eq!(percent_decode("%+1"), "%+1");
        assert_eq!(percent_decode("%C3%A9t%C3%A9"), "été");
    }

    #[test]
    fn control_characters_in_addresses_refuse_the_link() {
        for link in [
            "mailto:a@example.com%0D%0ARCPT%20TO:<b@example.com>",
            "mailto:?to=a@example.com%0A",
            "mailto:x@example.com?cc=a@b.example>%0D%0ARCPT%20TO:<evil@example.com",
            "mailto:x@example.com?bcc=a%00@example.com",
            "mailto:x@example.com?to=a%C2%85@example.com",
        ] {
            assert_eq!(parse_mailto(link), None, "{link}");
        }
        // The body may have line breaks; the subject loses them.
        let draft = parse_mailto("mailto:a@example.com?subject=Hi%0D%0AThere&body=One%0D%0ATwo")
            .expect("parses");
        assert_eq!(draft.subject, "Hi  There");
        assert_eq!(draft.body, "One\r\nTwo");
    }
}
