//! A small, defensive vCard reader for contact cards (RFC 6350).
//!
//! go-proton-api reads contact cards with go-vcard; Zmail only needs to
//! read fields by name and group, so this is Zmail's own minimal decoder
//! rather than a port: unfolded lines, `group.NAME;PARAM=VALUE:value`, and
//! the value escapes `\\`, `\n`, `\,` and `\;`.
//!
//! Cards come from the server, so the reader is strict about shape and
//! size and never panics: a card over [`MAX_CARD_BYTES`], a line over
//! [`MAX_LINE_BYTES`] once unfolded, more than [`MAX_FIELDS`] properties,
//! a line without a colon or a name, a missing `BEGIN:VCARD`, a nested
//! card, or a card without its `END:VCARD` is rejected as a whole.
//! Unfolding is linear in the input however deep the folding. Properties
//! Zmail does not read are kept but never interpreted. Callers verify a
//! signed card's signature before decoding it (`contact.rs`).

/// The largest card accepted, in bytes.
pub(crate) const MAX_CARD_BYTES: usize = 1024 * 1024;
/// The longest content line accepted once unfolded, in bytes. A pinned key
/// is the longest value a card holds.
pub(crate) const MAX_LINE_BYTES: usize = 256 * 1024;
/// The most properties a card may hold.
pub(crate) const MAX_FIELDS: usize = 1000;

/// One content line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Field {
    /// The group (`item1` in `item1.EMAIL`), or empty.
    pub(crate) group: String,
    /// The property name, upper-case.
    pub(crate) name: String,
    /// The value, unescaped.
    pub(crate) value: String,
}

/// A decoded card: its fields in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Card {
    fields: Vec<Field>,
}

impl Card {
    /// Decodes the first card in `text`; anything after its `END:VCARD` is
    /// ignored.
    pub(crate) fn decode(text: &str) -> Result<Self, String> {
        if text.len() > MAX_CARD_BYTES {
            return Err(format!(
                "the card is {} bytes, over the {MAX_CARD_BYTES}-byte limit",
                text.len()
            ));
        }
        let mut fields = Vec::new();
        let mut began = false;
        for line in unfold(text)? {
            if line.trim().is_empty() {
                continue;
            }
            let field = parse_line(&line)?;
            let is_card = field.value.eq_ignore_ascii_case("VCARD");
            match field.name.as_str() {
                "BEGIN" if is_card && began => return Err("a card inside a card".to_owned()),
                "BEGIN" if is_card => began = true,
                "END" if is_card && began => return Ok(Self { fields }),
                _ if !began => return Err("the card does not start with BEGIN:VCARD".to_owned()),
                _ => {
                    if fields.len() == MAX_FIELDS {
                        return Err(format!("the card has over {MAX_FIELDS} properties"));
                    }
                    fields.push(field);
                }
            }
        }
        Err(if began {
            "the card has no END:VCARD".to_owned()
        } else {
            "no vCard found".to_owned()
        })
    }

    /// The fields called `name`, in card order.
    pub(crate) fn get<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Field> + 'a {
        self.fields
            .iter()
            .filter(move |field| field.name.eq_ignore_ascii_case(name))
    }
}

/// Joins folded lines: a line break followed by a space or tab continues
/// the line, without that one space or tab.
fn unfold(text: &str) -> Result<Vec<String>, String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(continuation), Some(last)) => last.push_str(continuation),
            _ => lines.push(raw.to_owned()),
        }
        if lines.last().is_some_and(|line| line.len() > MAX_LINE_BYTES) {
            return Err(format!(
                "a card line is over the {MAX_LINE_BYTES}-byte limit"
            ));
        }
    }
    Ok(lines)
}

fn parse_line(line: &str) -> Result<Field, String> {
    // The name and parameters end at the first colon outside quotes.
    let mut quoted = false;
    let colon = line
        .char_indices()
        .find(|&(_, ch)| {
            if ch == '"' {
                quoted = !quoted;
            }
            ch == ':' && !quoted
        })
        .map(|(index, _)| index)
        .ok_or_else(|| "a card line without a value".to_owned())?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let name_and_group = head.split(';').next().unwrap_or_default();
    let (group, name) = match name_and_group.rsplit_once('.') {
        Some((group, name)) => (group.to_owned(), name),
        None => (String::new(), name_and_group),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err("a card line without a valid property name".to_owned());
    }
    Ok(Field {
        group,
        name: name.to_ascii_uppercase(),
        value: unescape(value),
    })
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_grouped_fields_with_parameters_and_folding() {
        let card = Card::decode(
            "BEGIN:VCARD\r\nVERSION:4.0\r\nitem1.EMAIL;PREF=1:alice@example.com\r\n\
             item1.KEY;PREF=1:data:application/pgp-keys;base64,QUJD\r\n  REVG\r\n\
             item1.X-PM-SIGN:true\r\nNOTE;X-QUOTED=\"a:b\":one\\, two\\nthree\r\nEND:VCARD\r\n",
        )
        .expect("card");
        let email = card.get("email").next().expect("email");
        assert_eq!(email.group, "item1");
        assert_eq!(email.value, "alice@example.com");
        assert_eq!(
            card.get("KEY").next().map(|field| field.value.as_str()),
            Some("data:application/pgp-keys;base64,QUJD REVG")
        );
        assert_eq!(
            card.get("NOTE").next().map(|field| field.value.as_str()),
            Some("one, two\nthree")
        );
        assert_eq!(card.get("X-PM-SIGN").count(), 1);
    }

    #[test]
    fn unfolds_deep_folding_linearly() {
        // 100,000 continuation lines of one character each.
        let folded = "\r\n x".repeat(100_000);
        let card =
            Card::decode(&format!("BEGIN:VCARD\r\nNOTE:{folded}\r\nEND:VCARD\r\n")).expect("card");
        assert_eq!(
            card.get("NOTE").next().map(|field| field.value.len()),
            Some(100_000)
        );
        // A tab folds too.
        let card = Card::decode("BEGIN:VCARD\r\nFN:A\r\n\tB\r\nEND:VCARD").expect("card");
        assert_eq!(
            card.get("FN").next().map(|field| field.value.as_str()),
            Some("AB")
        );
        // A continuation with nothing to continue is a malformed line.
        assert!(Card::decode(" stray\r\nBEGIN:VCARD\r\nEND:VCARD").is_err());
    }

    #[test]
    fn rejects_malformed_and_hostile_cards_without_panicking() {
        let huge_line = format!(
            "BEGIN:VCARD\r\nNOTE:{}\r\nEND:VCARD",
            "a".repeat(MAX_LINE_BYTES + 1)
        );
        let huge_folded = format!(
            "BEGIN:VCARD\r\nNOTE:{}\r\nEND:VCARD",
            "\r\n aaaa".repeat(MAX_LINE_BYTES / 4 + 1)
        );
        let huge_card = format!(
            "BEGIN:VCARD\r\n{}END:VCARD",
            "NOTE:x\r\n".repeat(MAX_CARD_BYTES / 8 + 1)
        );
        let many_fields = format!(
            "BEGIN:VCARD\r\n{}END:VCARD",
            "N:x\r\n".repeat(MAX_FIELDS + 1)
        );
        let cases = [
            "",
            "hello",
            "BEGIN:VCARD\r\nFN:unterminated\r\n",
            "FN:before begin\r\nBEGIN:VCARD\r\nEND:VCARD",
            "BEGIN:VCARD\r\nBEGIN:VCARD\r\nEND:VCARD\r\nEND:VCARD",
            "BEGIN:VCARD\r\nno colon here\r\nEND:VCARD",
            "BEGIN:VCARD\r\n:no name\r\nEND:VCARD",
            "BEGIN:VCARD\r\nitem1.:empty name\r\nEND:VCARD",
            "BEGIN:VCARD\r\nBAD NAME:x\r\nEND:VCARD",
            "BEGIN:VCARD\r\nNOTE;X=\"unclosed:value\r\nEND:VCARD",
            "END:VCARD",
            &huge_line,
            &huge_folded,
            &huge_card,
            &many_fields,
        ];
        for case in cases {
            assert!(
                Card::decode(case).is_err(),
                "accepted: {:?}",
                &case[..case.len().min(80)]
            );
        }
        // Odd but harmless input decodes.
        let card = Card::decode(
            "BEGIN:VCARD\r\nX-UNKNOWN;A=1:ignored\\\r\nÉ:\r\nEND:VCARD\r\nGARBAGE AFTER",
        );
        assert!(card.is_err(), "non-ASCII property names are refused");
        let card = Card::decode("BEGIN:VCARD\r\nX-UNKNOWN;A=1:trailing\\\r\nEND:VCARD\r\nafter")
            .expect("card");
        assert_eq!(
            card.get("X-UNKNOWN")
                .next()
                .map(|field| field.value.as_str()),
            Some("trailing\\")
        );
    }
}
