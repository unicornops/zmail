//! Records as servers return them: every property optional, nothing the
//! server sent lost, and no shape of JSON object a decoding error.
//!
//! Servers differ on `null` against leaving a property out, return only
//! the properties a call asked for, and now and then send a value of
//! another type than the RFC gives the property. A record declared with
//! [`record!`] reads all of that:
//!
//! - a property left out, or `null`, is `None`;
//! - a property whose value is not of the RFC's type is `None` too, and
//!   its JSON is kept in the record's `extra` under the property's name,
//!   so one odd value costs that property and not the whole response;
//! - properties the record has no field for are kept in `extra`.
//!
//! Only something that is not a JSON object at all fails to decode as a
//! record.

use std::collections::BTreeMap;

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

/// The properties of a record being decoded, taken out one at a time.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(crate) struct Fields(BTreeMap<String, Value>);

impl Fields {
    /// The property `name` as `T`; `None` when it is missing, `null` or
    /// not a `T`. A value that is not a `T` stays among the rest.
    pub(crate) fn take<T: DeserializeOwned>(&mut self, name: &str) -> Option<T> {
        let value = self.0.remove(name)?;
        if value.is_null() {
            return None;
        }
        match T::deserialize(&value) {
            Ok(typed) => Some(typed),
            Err(_) => {
                self.0.insert(name.to_owned(), value);
                None
            }
        }
    }

    /// Whatever was not taken.
    pub(crate) fn rest(self) -> BTreeMap<String, Value> {
        self.0
    }
}

/// Declares a record a server returns: a struct whose fields are all
/// `Option`s, with an `extra` map, decoded as the [module](self) says
/// and written back with the properties that are `None` left out.
macro_rules! record {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $json:literal => $field:ident: $ty:ty,
            )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq)]
        #[non_exhaustive]
        pub struct $name {
            $(
                $(#[$field_meta])*
                pub $field: Option<$ty>,
            )*
            /// Properties this crate has no field for, and any property
            /// above whose value was not of the type the RFC gives it,
            /// as the JSON the server sent.
            pub extra: std::collections::BTreeMap<String, serde_json::Value>,
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                let mut fields =
                    <$crate::record::Fields as serde::Deserialize>::deserialize(deserializer)?;
                Ok(Self {
                    $($field: fields.take($json),)*
                    extra: fields.rest(),
                })
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                use serde::ser::SerializeMap as _;

                let mut map = serializer.serialize_map(None)?;
                $(
                    if let Some(value) = &self.$field {
                        map.serialize_entry($json, value)?;
                    }
                )*
                for (name, value) in &self.extra {
                    map.serialize_entry(name, value)?;
                }
                map.end()
            }
        }
    };
}
pub(crate) use record;

/// A value that is `null`, missing or of another type than expected, as
/// the type's default: for the parts of an answer that say something
/// about it (a position, a total) and are not worth failing over.
pub(crate) fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(T::deserialize(value).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    record! {
        /// A record for the tests.
        pub struct Sample {
            /// A string.
            "name" => name: String,
            /// A number.
            "sortOrder" => sort_order: u32,
            /// A nested list.
            "children" => children: Vec<Sample>,
        }
    }

    #[test]
    fn missing_null_and_mistyped_properties_are_none_and_nothing_is_lost() {
        let sample: Sample = serde_json::from_value(json!({
            "name": null,
            "sortOrder": "seven",
            "children": [{"name": "child", "sortOrder": 2, "later": true}],
            "futureProperty": {"a": 1}
        }))
        .expect("decodes");
        assert_eq!(sample.name, None);
        assert_eq!(sample.sort_order, None);
        assert_eq!(sample.extra["sortOrder"], "seven", "the odd value is kept");
        assert_eq!(sample.extra["futureProperty"], json!({"a": 1}));
        assert!(!sample.extra.contains_key("name"), "a null is not kept");
        let child = &sample.children.as_ref().expect("children")[0];
        assert_eq!(child.name.as_deref(), Some("child"));
        assert_eq!(child.sort_order, Some(2));
        assert_eq!(child.extra["later"], true);

        // Written back, the nulls are gone and the rest is as it came.
        assert_eq!(
            serde_json::to_value(&sample).expect("encodes"),
            json!({
                "sortOrder": "seven",
                "children": [{"name": "child", "sortOrder": 2, "later": true}],
                "futureProperty": {"a": 1}
            })
        );
    }

    #[test]
    fn only_something_that_is_no_object_fails() {
        assert_eq!(
            serde_json::from_value::<Sample>(json!({})).expect("decodes"),
            Sample::default()
        );
        for not_a_record in [json!(null), json!([]), json!("text"), json!(3)] {
            assert!(serde_json::from_value::<Sample>(not_a_record).is_err());
        }
        // A list with something that is no record in it is not a list of
        // records: the property is kept as it came.
        let sample: Sample =
            serde_json::from_value(json!({"children": [{"name": "a"}, 7]})).expect("decodes");
        assert_eq!(sample.children, None);
        assert_eq!(sample.extra["children"], json!([{"name": "a"}, 7]));
    }

    #[test]
    fn lenient_values_fall_back_to_the_default() {
        #[derive(Deserialize)]
        struct Answer {
            #[serde(default, deserialize_with = "lenient")]
            position: u64,
            #[serde(default, deserialize_with = "lenient")]
            total: Option<u64>,
        }
        for (value, position, total) in [
            (json!({"position": 3, "total": 9}), 3, Some(9)),
            (json!({"position": null, "total": null}), 0, None),
            (json!({"position": -1, "total": "many"}), 0, None),
            (json!({}), 0, None),
        ] {
            let answer: Answer = serde_json::from_value(value).expect("decodes");
            assert_eq!((answer.position, answer.total), (position, total));
        }
    }
}
