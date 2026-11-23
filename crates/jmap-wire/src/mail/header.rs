//! Header fields as properties (RFC 8621 sections 4.1.2 and 4.1.3):
//! `header:{name}:as{Form}:all`, and reading what comes back for one.

use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{Date, EmailAddress, EmailAddressGroup};

/// The forms a header field can be fetched in (RFC 8621 section 4.1.2).
/// A server refuses a form the RFC does not allow for the field
/// (`header:From:asDate`, say) with `invalidArguments`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HeaderForm {
    /// The field's value as it is in the message, usually with a leading
    /// space (section 4.1.2.1). Any field.
    Raw,
    /// Unfolded, decoded text (section 4.1.2.2): `Subject`, `Comments`,
    /// `Keywords`, `List-Id` and fields outside RFC 5322 and RFC 2369.
    Text,
    /// A list of addresses, groups flattened (section 4.1.2.3): `From`,
    /// `Sender`, `Reply-To`, `To`, `Cc`, `Bcc`, their `Resent-` forms
    /// and fields outside RFC 5322 and RFC 2369.
    Addresses,
    /// A list of address groups (section 4.1.2.4): the same fields.
    GroupedAddresses,
    /// A list of message ids without their angle brackets (section
    /// 4.1.2.5): `Message-ID`, `In-Reply-To`, `References`,
    /// `Resent-Message-ID` and fields outside RFC 5322 and RFC 2369.
    MessageIds,
    /// A date (section 4.1.2.6): `Date`, `Resent-Date` and fields
    /// outside RFC 5322 and RFC 2369.
    Date,
    /// A list of URLs (section 4.1.2.7): the `List-` fields of RFC 2369
    /// and fields outside RFC 5322 and RFC 2369.
    Urls,
}

impl HeaderForm {
    /// The form's name in a property (`Text` in `:asText`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "Raw",
            Self::Text => "Text",
            Self::Addresses => "Addresses",
            Self::GroupedAddresses => "GroupedAddresses",
            Self::MessageIds => "MessageIds",
            Self::Date => "Date",
            Self::Urls => "URLs",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [
            Self::Raw,
            Self::Text,
            Self::Addresses,
            Self::GroupedAddresses,
            Self::MessageIds,
            Self::Date,
            Self::Urls,
        ]
        .into_iter()
        .find(|form| form.as_str().eq_ignore_ascii_case(name))
    }
}

/// A header field as an Email (or body part) property (RFC 8621 section
/// 4.1.3): `header:{name}`, in a form, for the last instance of the
/// field or for all of them.
///
/// ```
/// use jmap_wire::mail::HeaderProperty;
///
/// assert_eq!(HeaderProperty::raw("X-Spam-Score").to_string(), "header:X-Spam-Score");
/// assert_eq!(
///     HeaderProperty::addresses("Resent-To").all().to_string(),
///     "header:Resent-To:asAddresses:all"
/// );
/// assert_eq!(
///     "header:List-Post:asURLs".parse::<HeaderProperty>()?,
///     HeaderProperty::urls("List-Post")
/// );
/// # Ok::<(), jmap_wire::mail::NotAHeaderProperty>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HeaderProperty {
    name: String,
    form: HeaderForm,
    all: bool,
}

impl HeaderProperty {
    /// The last instance of the field `name` in `form`.
    ///
    /// A field name is printable ASCII without a colon; this does not
    /// check it, and a server refuses a property it cannot read.
    pub fn new(name: impl Into<String>, form: HeaderForm) -> Self {
        Self {
            name: name.into(),
            form,
            all: false,
        }
    }

    /// The field as it is in the message ([`HeaderForm::Raw`]).
    pub fn raw(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::Raw)
    }

    /// The field as text ([`HeaderForm::Text`]).
    pub fn text(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::Text)
    }

    /// The field as addresses ([`HeaderForm::Addresses`]).
    pub fn addresses(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::Addresses)
    }

    /// The field as address groups ([`HeaderForm::GroupedAddresses`]).
    pub fn grouped_addresses(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::GroupedAddresses)
    }

    /// The field as message ids ([`HeaderForm::MessageIds`]).
    pub fn message_ids(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::MessageIds)
    }

    /// The field as a date ([`HeaderForm::Date`]).
    pub fn date(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::Date)
    }

    /// The field as URLs ([`HeaderForm::Urls`]).
    pub fn urls(name: impl Into<String>) -> Self {
        Self::new(name, HeaderForm::Urls)
    }

    /// Every instance of the field, in the order they are in the
    /// message (`:all`), instead of the last.
    pub fn all(mut self) -> Self {
        self.all = true;
        self
    }

    /// The field's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The form asked for.
    pub fn form(&self) -> HeaderForm {
        self.form
    }

    /// Whether every instance is asked for.
    pub fn is_all(&self) -> bool {
        self.all
    }

    /// Whether `other` is the same property: field names are matched
    /// without regard to case (section 4.1.3).
    fn matches(&self, other: &Self) -> bool {
        self.form == other.form
            && self.all == other.all
            && self.name.eq_ignore_ascii_case(&other.name)
    }
}

impl fmt::Display for HeaderProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "header:{}", self.name)?;
        if self.form != HeaderForm::Raw {
            write!(f, ":as{}", self.form.as_str())?;
        }
        if self.all {
            f.write_str(":all")?;
        }
        Ok(())
    }
}

impl From<HeaderProperty> for String {
    fn from(property: HeaderProperty) -> Self {
        property.to_string()
    }
}

/// A property name that is not a header field property.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a header field property (header:Name[:asForm][:all])")]
pub struct NotAHeaderProperty(String);

impl FromStr for HeaderProperty {
    type Err = NotAHeaderProperty;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let not = || NotAHeaderProperty(text.to_owned());
        let mut parts = text.split(':');
        if !parts
            .next()
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("header"))
        {
            return Err(not());
        }
        let name = parts
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(not)?;
        let mut property = Self::raw(name);
        let mut suffix = parts.next();
        if let Some(form) = suffix
            .and_then(|suffix| suffix.get(..2).zip(suffix.get(2..)))
            .filter(|(prefix, _)| prefix.eq_ignore_ascii_case("as"))
            .and_then(|(_, form)| HeaderForm::parse(form))
        {
            property.form = form;
            suffix = parts.next();
        }
        if suffix.is_some_and(|suffix| suffix.eq_ignore_ascii_case("all")) {
            property.all = true;
            suffix = parts.next();
        }
        match suffix {
            None => Ok(property),
            Some(_) => Err(not()),
        }
    }
}

/// A form a header field is read in: the type parameter of
/// [`EmailRecord::header`](super::EmailRecord::header) and
/// [`header_all`](super::EmailRecord::header_all). The forms are
/// [`AsRaw`], [`AsText`], [`AsAddresses`], [`AsGroupedAddresses`],
/// [`AsMessageIds`], [`AsDate`] and [`AsUrls`].
pub trait ParsedForm {
    /// The form.
    const FORM: HeaderForm;
    /// What one instance of the field is in this form.
    type Value: DeserializeOwned;
}

macro_rules! parsed_forms {
    ($($(#[$meta:meta])* $name:ident => $form:ident, $value:ty;)*) => {
        $(
            $(#[$meta])*
            #[derive(Debug, Clone, Copy)]
            pub enum $name {}

            impl ParsedForm for $name {
                const FORM: HeaderForm = HeaderForm::$form;
                type Value = $value;
            }
        )*
    };
}

parsed_forms! {
    /// [`HeaderForm::Raw`]: a `String`.
    AsRaw => Raw, String;
    /// [`HeaderForm::Text`]: a `String`.
    AsText => Text, String;
    /// [`HeaderForm::Addresses`]: a list of [`EmailAddress`]es.
    AsAddresses => Addresses, Vec<EmailAddress>;
    /// [`HeaderForm::GroupedAddresses`]: a list of
    /// [`EmailAddressGroup`]s.
    AsGroupedAddresses => GroupedAddresses, Vec<EmailAddressGroup>;
    /// [`HeaderForm::MessageIds`]: a list of ids.
    AsMessageIds => MessageIds, Vec<String>;
    /// [`HeaderForm::Date`]: a [`Date`].
    AsDate => Date, Date;
    /// [`HeaderForm::Urls`]: a list of URLs.
    AsUrls => Urls, Vec<String>;
}

/// The value of `property` among a record's properties without a field.
/// The name the server answers with should be the one asked for, letter
/// for letter (RFC 8621 section 4.2); one that differs in case or
/// spells out `:asRaw` is found too.
pub(crate) fn find<'a>(
    properties: &'a BTreeMap<String, Value>,
    property: &HeaderProperty,
) -> Option<&'a Value> {
    properties.get(&property.to_string()).or_else(|| {
        properties.iter().find_map(|(name, value)| {
            name.parse::<HeaderProperty>()
                .is_ok_and(|named| named.matches(property))
                .then_some(value)
        })
    })
}

fn decode<T: DeserializeOwned>(value: &Value) -> Option<T> {
    T::deserialize(value).ok()
}

/// The last instance of the field `name` in form `F`.
pub(crate) fn last<F: ParsedForm>(
    properties: &BTreeMap<String, Value>,
    name: &str,
) -> Option<F::Value> {
    let property = HeaderProperty::new(name, F::FORM);
    match find(properties, &property) {
        Some(value) => decode(value),
        // Only every instance was fetched: the last of them.
        None => decode(find(properties, &property.all())?.as_array()?.last()?),
    }
}

/// Every instance of the field `name` in form `F`.
pub(crate) fn all<F: ParsedForm>(
    properties: &BTreeMap<String, Value>,
    name: &str,
) -> Option<Vec<F::Value>> {
    let property = HeaderProperty::new(name, F::FORM);
    let one = |value: &Value| match value {
        Value::Null => Some(Vec::new()),
        value => decode(value).map(|value| vec![value]),
    };
    match find(properties, &property.clone().all()) {
        Some(Value::Array(instances)) => instances
            .iter()
            .filter(|instance| !instance.is_null())
            .map(decode)
            .collect::<Option<Vec<_>>>()
            // A server that answered `:all` with the one value.
            .or_else(|| decode(&Value::Array(instances.clone())).map(|value| vec![value])),
        Some(value) => one(value),
        // Only the last instance was fetched.
        None => one(find(properties, &property)?),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn property_names_are_written_and_parsed_as_rfc_8621_section_4_1_3_has_them() {
        let cases = [
            (HeaderProperty::raw("subject"), "header:subject"),
            (HeaderProperty::raw("Received").all(), "header:Received:all"),
            (HeaderProperty::text("Subject"), "header:Subject:asText"),
            (
                HeaderProperty::addresses("Resent-To").all(),
                "header:Resent-To:asAddresses:all",
            ),
            (
                HeaderProperty::grouped_addresses("To"),
                "header:To:asGroupedAddresses",
            ),
            (
                HeaderProperty::message_ids("References"),
                "header:References:asMessageIds",
            ),
            (HeaderProperty::date("Date"), "header:Date:asDate"),
            (HeaderProperty::urls("List-POST"), "header:List-POST:asURLs"),
        ];
        for (property, text) in cases {
            assert_eq!(property.to_string(), text);
            assert_eq!(text.parse::<HeaderProperty>().expect("parses"), property);
        }
        // Raw spelt out, and other letter case, are the same properties.
        assert_eq!(
            "HEADER:X-A:asRaw:ALL"
                .parse::<HeaderProperty>()
                .expect("parses"),
            HeaderProperty::raw("X-A").all()
        );
        assert_eq!(
            "header:X-A:asurls"
                .parse::<HeaderProperty>()
                .expect("parses"),
            HeaderProperty::urls("X-A")
        );
        for not in [
            "subject",
            "header",
            "header:",
            "header:X:asNothing",
            "header:X:all:asText",
            "header:X:asText:all:more",
            "headers:X",
        ] {
            assert!(not.parse::<HeaderProperty>().is_err(), "{not}");
        }
        // A field named like a suffix is still a field.
        assert_eq!(
            "header:all".parse::<HeaderProperty>().expect("parses"),
            HeaderProperty::raw("all")
        );
        assert_eq!(HeaderProperty::text("X-A").all().name(), "X-A",);
    }

    fn properties(value: Value) -> BTreeMap<String, Value> {
        serde_json::from_value(value).expect("an object")
    }

    #[test]
    fn a_field_is_read_in_the_form_it_was_fetched_in() {
        let fetched = properties(json!({
            "header:Subject:asText": "Dinner on Thursday?",
            "header:X-Probe": " two",
            "header:X-Probe:all": [" one", " two"],
            "header:Comments:asText:all": ["first", "second"],
            "header:To:asAddresses": [{"name": "Joe", "email": "joe@example.com"}],
            "header:Resent-To:asAddresses:all": [
                [{"name": null, "email": "a@example.com"}],
                [{"name": null, "email": "b@example.com"}, {"name": "C", "email": "c@example.com"}]
            ],
            "header:To:asGroupedAddresses": [
                {"name": null, "addresses": [{"name": "James Smythe", "email": "james@example.com"}]},
                {"name": "Friends", "addresses": [
                    {"name": null, "email": "jane@example.com"},
                    {"name": "John Smith", "email": "john@example.com"}
                ]}
            ],
            "header:References:asMessageIds": ["a@example.com", "b@example.com"],
            "header:Date:asDate": "2013-10-13T14:12:00+02:00",
            "header:List-POST:asURLs": ["mailto:partytime@lists.example.com"],
            "header:X-Absent": null,
            "header:X-Absent:all": [],
            "header:X-Unparsed:asMessageIds:all": [["a@example.com"], null]
        }));
        assert_eq!(
            last::<AsText>(&fetched, "Subject").as_deref(),
            Some("Dinner on Thursday?")
        );
        // Field names match whatever their case.
        assert_eq!(last::<AsRaw>(&fetched, "x-probe").as_deref(), Some(" two"));
        assert_eq!(
            all::<AsRaw>(&fetched, "X-PROBE").expect("fetched"),
            [" one", " two"]
        );
        assert_eq!(
            all::<AsText>(&fetched, "Comments").expect("fetched"),
            ["first", "second"]
        );
        assert_eq!(
            last::<AsAddresses>(&fetched, "To").expect("fetched"),
            [EmailAddress::named("Joe", "joe@example.com")]
        );
        let resent = all::<AsAddresses>(&fetched, "Resent-To").expect("fetched");
        assert_eq!((resent.len(), resent[1].len()), (2, 2));
        // The grouped example of RFC 8621 section 4.1.2.4.
        let groups = last::<AsGroupedAddresses>(&fetched, "To").expect("fetched");
        assert_eq!(groups[0].name, None);
        assert_eq!(groups[1].name.as_deref(), Some("Friends"));
        assert_eq!(groups[1].addresses[1].email, "john@example.com");
        assert_eq!(
            last::<AsMessageIds>(&fetched, "References").expect("fetched"),
            ["a@example.com", "b@example.com"]
        );
        assert_eq!(
            last::<AsDate>(&fetched, "Date").expect("fetched").as_str(),
            "2013-10-13T14:12:00+02:00"
        );
        assert_eq!(
            last::<AsUrls>(&fetched, "List-Post").expect("fetched"),
            ["mailto:partytime@lists.example.com"]
        );

        // A field the message does not have: null for the last, no
        // instances for all.
        assert_eq!(last::<AsRaw>(&fetched, "X-Absent"), None);
        assert_eq!(all::<AsRaw>(&fetched, "X-Absent"), Some(Vec::new()));
        // An instance the server could not parse is left out.
        assert_eq!(
            all::<AsMessageIds>(&fetched, "X-Unparsed").expect("fetched"),
            [["a@example.com"]]
        );
        // Not fetched at all, or fetched in another form.
        assert_eq!(last::<AsRaw>(&fetched, "X-Other"), None);
        assert_eq!(all::<AsRaw>(&fetched, "X-Other"), None);
        assert_eq!(last::<AsText>(&fetched, "X-Probe"), None);
        assert_eq!(last::<AsDate>(&fetched, "Subject"), None);
    }

    #[test]
    fn the_last_and_all_forms_stand_in_for_each_other() {
        let only_all = properties(json!({"header:Received:all": [" a", " b"]}));
        assert_eq!(last::<AsRaw>(&only_all, "Received").as_deref(), Some(" b"));
        let only_last = properties(json!({
            "header:Received": " b",
            "header:X-None:asText": null
        }));
        assert_eq!(
            all::<AsRaw>(&only_last, "Received").expect("fetched"),
            [" b"]
        );
        assert_eq!(all::<AsText>(&only_last, "X-None"), Some(Vec::new()));

        // A server that answers `:all` with the one value, or with null.
        let odd = properties(json!({
            "header:Delivered-To:all": " me@example.com",
            "header:To:asAddresses:all": [{"name": null, "email": "a@example.com"}],
            "header:X-Null:all": null,
            "header:X-Number:all": 7
        }));
        assert_eq!(
            all::<AsRaw>(&odd, "Delivered-To").expect("read"),
            [" me@example.com"]
        );
        assert_eq!(
            all::<AsAddresses>(&odd, "To").expect("read"),
            [[EmailAddress::new("a@example.com")]]
        );
        assert_eq!(all::<AsRaw>(&odd, "X-Null"), Some(Vec::new()));
        assert_eq!(all::<AsRaw>(&odd, "X-Number"), None);
    }
}
