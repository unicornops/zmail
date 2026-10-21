// Ported from go-proton-api `header_types.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-21: rewritten in Rust with a serde
// visitor, which sees object keys in document order, instead of an
// ordered-JSON library. Serialization is not ported.

//! Parsed message headers, in the order the message has them.

use std::fmt;

use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};

/// Headers as the API sends them: an object of name to value or values,
/// read in order (go-proton-api's `Headers`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    /// `(name, values)` in the order of the message.
    pub fields: Vec<(String, Vec<String>)>,
}

impl Headers {
    /// The values of a header, compared case-insensitively.
    pub fn values(&self, name: &str) -> &[String] {
        self.fields
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, values)| values.as_slice())
            .unwrap_or_default()
    }
}

impl<'de> Deserialize<'de> for Headers {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct HeadersVisitor;

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            One(String),
            Many(Vec<String>),
        }

        impl<'de> Visitor<'de> for HeadersVisitor {
            type Value = Headers;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an object of header names to strings or string lists")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Headers, E> {
                Ok(Headers::default())
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Headers, A::Error> {
                let mut fields = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, Value>()? {
                    let values = match value {
                        Value::One(value) => vec![value],
                        Value::Many(values) => values,
                    };
                    fields.push((name, values));
                }
                Ok(Headers { fields })
            }
        }

        deserializer.deserialize_any(HeadersVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_header_order_and_lists() {
        let headers: Headers = serde_json::from_str(
            r#"{"Received": ["a", "b"], "Subject": "Hi", "X-Pm-Origin": "internal", "Content-Type": "text/plain"}"#,
        )
        .expect("headers");
        let names: Vec<&str> = headers
            .fields
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Received", "Subject", "X-Pm-Origin", "Content-Type"]
        );
        assert_eq!(headers.values("received"), ["a", "b"]);
        assert_eq!(headers.values("subject"), ["Hi"]);
        assert!(headers.values("missing").is_empty());
        assert!(serde_json::from_str::<Headers>(r#"{"X": 1}"#).is_err());
        assert_eq!(
            serde_json::from_str::<Headers>("null").expect("null"),
            Headers::default()
        );
    }
}
