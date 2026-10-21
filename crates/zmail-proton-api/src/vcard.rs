//! A small vCard reader for contact cards (RFC 6350).
//!
//! go-proton-api reads contact cards with go-vcard; Zmail only needs to
//! read fields by name and group, so this is Zmail's own minimal decoder
//! rather than a port: unfolded lines, `group.NAME;PARAM=VALUE:value`, and
//! the value escapes `\\`, `\n`, `\,` and `\;`.

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
    /// Decodes the first card in `text`.
    pub(crate) fn decode(text: &str) -> Result<Self, String> {
        let mut fields = Vec::new();
        let mut began = false;
        for line in unfold(text) {
            if line.trim().is_empty() {
                continue;
            }
            let field = parse_line(&line)?;
            match (
                field.name.as_str(),
                field.value.to_ascii_uppercase().as_str(),
            ) {
                ("BEGIN", "VCARD") => began = true,
                ("END", "VCARD") => break,
                _ if began => fields.push(field),
                _ => return Err("the card does not start with BEGIN:VCARD".to_owned()),
            }
        }
        if !began {
            return Err("no vCard found".to_owned());
        }
        Ok(Self { fields })
    }

    /// The fields called `name`.
    pub(crate) fn get<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Field> + 'a {
        self.fields
            .iter()
            .filter(move |field| field.name.eq_ignore_ascii_case(name))
    }
}

/// Joins folded lines: a line break followed by a space or tab continues
/// the line.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match (raw.strip_prefix([' ', '\t']), lines.last_mut()) {
            (Some(continuation), Some(last)) => last.push_str(continuation),
            _ => lines.push(raw.to_owned()),
        }
    }
    lines
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
        .ok_or_else(|| format!("a vCard line without a value: {line:?}"))?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let name_and_group = head.split(';').next().unwrap_or_default();
    let (group, name) = match name_and_group.rsplit_once('.') {
        Some((group, name)) => (group.to_owned(), name),
        None => (String::new(), name_and_group),
    };
    if name.is_empty() {
        return Err(format!("a vCard line without a name: {line:?}"));
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
    fn rejects_text_that_is_not_a_card() {
        assert!(Card::decode("hello").is_err());
        assert!(Card::decode("").is_err());
    }
}
