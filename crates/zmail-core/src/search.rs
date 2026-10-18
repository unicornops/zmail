//! The mail search query language.
//!
//! A query is a list of whitespace-separated clauses that must all match:
//!
//! | Clause | Matches |
//! |---|---|
//! | `word` | any indexed text starting with `word` |
//! | `"exact phrase"` | the words in that order |
//! | `from:alice`, `from:"Alice Smith"` | the sender |
//! | `to:bob` | the recipients |
//! | `subject:report` | the subject line |
//! | `has:attachment` | threads with an attachment |
//! | `is:unread`, `is:read`, `is:flagged` (`is:starred`) | thread state |
//! | `in:inbox`, `in:"Project X"` | a mailbox, by name or role |
//! | `after:2026-01-31`, `before:2026-02-01` | message dates (`YYYY-MM-DD`) |
//! | `after:7d`, `before:2w` | relative dates: days, weeks, months, years ago |
//!
//! A leading `-` negates any clause (`-word`, `-from:alice`, `-is:unread`).
//! Operator names are case-insensitive. A clause that does not parse as an
//! operator (an unknown name like `foo:bar`, an unknown `is:` value or an
//! invalid date) is kept as plain text, so nothing the user typed is
//! silently dropped.
//!
//! [`SearchQuery::parse`] is pure: it never touches SQL or a server. The
//! local store compiles the parsed clauses into parameterised SQL and an
//! escaped FTS5 expression, and the protocol adapters map them onto IMAP
//! `SEARCH` keys and JMAP `Email/query` filters.

use chrono::{Days, Months, NaiveDate, NaiveTime, TimeDelta, TimeZone};

/// A parsed search query: every clause must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchQuery {
    /// Clauses in the order they were typed.
    pub clauses: Vec<SearchClause>,
}

/// One clause of a query, optionally negated with a leading `-`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchClause {
    /// Whether the clause was written with a leading `-`.
    pub negated: bool,
    /// What the clause matches.
    pub term: SearchTerm,
}

/// What a single clause matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchTerm {
    /// Free text matched against every indexed field.
    Text(SearchText),
    /// Text matched against the sender.
    From(SearchText),
    /// Text matched against the recipients.
    To(SearchText),
    /// Text matched against the subject line.
    Subject(SearchText),
    /// The thread has at least one attachment.
    HasAttachment,
    /// The thread has unread messages.
    Unread,
    /// Every message in the thread is read.
    Read,
    /// The thread is flagged.
    Flagged,
    /// The thread is in the mailbox with this name or role.
    Mailbox(String),
    /// A message in the thread arrived before the start of this day.
    Before(DateSpec),
    /// A message in the thread arrived on or after the start of this day.
    After(DateSpec),
}

impl SearchTerm {
    /// Whether matching the term needs message text, which only the
    /// full-text index (or a server search) can answer.
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            Self::Text(_) | Self::From(_) | Self::To(_) | Self::Subject(_)
        )
    }
}

/// The text of a word or phrase clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchText {
    /// The text as typed, without surrounding quotes.
    pub value: String,
    /// Whether the text was quoted. Quoted text matches as an exact phrase;
    /// an unquoted word also matches longer words it is a prefix of.
    pub phrase: bool,
}

impl SearchText {
    fn word(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            phrase: false,
        }
    }

    /// Whether the text contains anything a search index can match on:
    /// at least one letter or digit. Punctuation-only text tokenizes to
    /// nothing, so callers skip it rather than match everything.
    pub fn is_searchable(&self) -> bool {
        self.value.chars().any(char::is_alphanumeric)
    }
}

/// A day boundary for `before:` and `after:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSpec {
    /// A calendar day, written `YYYY-MM-DD`.
    Day(NaiveDate),
    /// A day relative to today, written `7d`, `2w`, `3m` or `1y`.
    Ago {
        /// How many units back.
        amount: u32,
        /// The unit.
        unit: DateUnit,
    },
}

/// Unit of a relative [`DateSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateUnit {
    /// `d`
    Days,
    /// `w`
    Weeks,
    /// `m`
    Months,
    /// `y`
    Years,
}

impl DateSpec {
    /// Resolves the boundary to a calendar day, given today's date in the
    /// viewer's time zone. Returns `None` when the result falls outside
    /// chrono's date range.
    pub fn resolve(self, today: NaiveDate) -> Option<NaiveDate> {
        match self {
            Self::Day(day) => Some(day),
            Self::Ago { amount, unit } => match unit {
                DateUnit::Days => today.checked_sub_days(Days::new(u64::from(amount))),
                DateUnit::Weeks => today.checked_sub_days(Days::new(u64::from(amount) * 7)),
                DateUnit::Months => today.checked_sub_months(Months::new(amount)),
                DateUnit::Years => today.checked_sub_months(Months::new(amount.checked_mul(12)?)),
            },
        }
    }

    fn parse(value: &str) -> Option<Self> {
        if let Ok(day) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
            return Some(Self::Day(day));
        }
        let unit = match value.chars().last()?.to_ascii_lowercase() {
            'd' => DateUnit::Days,
            'w' => DateUnit::Weeks,
            'm' => DateUnit::Months,
            'y' => DateUnit::Years,
            _ => return None,
        };
        let digits = &value[..value.len() - 1];
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        Some(Self::Ago {
            amount: digits.parse().ok()?,
            unit,
        })
    }
}

impl SearchQuery {
    /// Parses a query typed by the user. Never fails: anything that is not
    /// a recognised operator becomes plain text.
    pub fn parse(input: &str) -> Self {
        let mut clauses = Vec::new();
        let mut rest = input;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let (clause, remaining) = parse_clause(rest);
            rest = remaining;
            if let Some(clause) = clause {
                clauses.push(clause);
            }
        }
        Self { clauses }
    }

    /// Whether the query has no clauses at all.
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// The value of the first positive `in:` clause, if the query names a
    /// mailbox. Such a clause chooses the folder, so it takes precedence
    /// over the folder a search would otherwise start in.
    pub fn mailbox_clause(&self) -> Option<&str> {
        self.clauses.iter().find_map(|clause| match &clause.term {
            SearchTerm::Mailbox(value) if !clause.negated => Some(value.as_str()),
            _ => None,
        })
    }

    /// Whether any clause needs message text (see [`SearchTerm::is_text`]).
    pub fn has_text_terms(&self) -> bool {
        self.clauses.iter().any(|clause| clause.term.is_text())
    }

    /// Whether answering the query fully needs message bodies: it has a
    /// positive free-text word or phrase. Header fields (`from:`, `to:`,
    /// `subject:`) and state are always stored locally, so only these
    /// queries can miss mail whose body is not indexed yet.
    pub fn needs_message_bodies(&self) -> bool {
        self.clauses.iter().any(|clause| {
            !clause.negated
                && matches!(&clause.term, SearchTerm::Text(text) if text.is_searchable())
        })
    }
}

/// The mailbox role an `in:` value also matches, for the common names that
/// differ from the stored role (`in:spam` finds the `junk` mailbox).
pub fn mailbox_role_alias(value: &str) -> &str {
    if value.eq_ignore_ascii_case("spam") {
        "junk"
    } else if value.eq_ignore_ascii_case("bin") {
        "trash"
    } else {
        value
    }
}

/// Whether an `in:` value names a mailbox, by name or role, ignoring
/// ASCII case.
pub fn mailbox_matches(value: &str, name: &str, role: Option<&str>) -> bool {
    name.eq_ignore_ascii_case(value)
        || role.is_some_and(|role| role.eq_ignore_ascii_case(mailbox_role_alias(value)))
}

/// The Unix time at which `day` starts in `zone`. Days that skip midnight
/// (a daylight-saving jump) start at the first valid local time after it.
pub fn day_start_unix<Tz: TimeZone>(day: NaiveDate, zone: &Tz) -> Option<i64> {
    let midnight = day.and_time(NaiveTime::MIN);
    (0..=2)
        .find_map(|hours| {
            zone.from_local_datetime(&(midnight + TimeDelta::hours(hours)))
                .earliest()
        })
        .map(|start| start.timestamp())
}

/// Parses one clause from the start of `input` (which has no leading
/// whitespace) and returns it with the unparsed remainder.
fn parse_clause(input: &str) -> (Option<SearchClause>, &str) {
    let (negated, body) = match input.strip_prefix('-') {
        Some(body) if body.starts_with(|ch: char| !ch.is_whitespace()) => (true, body),
        // A lone `-` is not a clause.
        Some(body) => return (None, body),
        None => (false, input),
    };

    if body.starts_with('"') {
        let (text, rest) = take_quoted(body);
        if text.is_empty() {
            return (None, rest);
        }
        let clause = SearchClause {
            negated,
            term: SearchTerm::Text(SearchText {
                value: text,
                phrase: true,
            }),
        };
        return (Some(clause), rest);
    }

    // `name:value`, where the value may be quoted (`from:"Alice Smith"`).
    if let Some(colon) = body.find(':') {
        let name = &body[..colon];
        let after = &body[colon + 1..];
        if !name.is_empty()
            && !name.contains(char::is_whitespace)
            && is_operator(name)
            && !after.starts_with(char::is_whitespace)
        {
            let (value, phrase, rest) = if after.starts_with('"') {
                let (value, rest) = take_quoted(after);
                (value, true, rest)
            } else {
                let (word, rest) = take_word(after);
                (word.to_owned(), false, rest)
            };
            if value.is_empty() {
                // `from:` with nothing after it narrows nothing.
                return (None, rest);
            }
            let term = operator_term(name, &value, phrase).unwrap_or_else(|| {
                SearchTerm::Text(SearchText {
                    value: format!("{name}:{value}"),
                    phrase,
                })
            });
            return (Some(SearchClause { negated, term }), rest);
        }
    }

    let (word, rest) = take_word(body);
    (
        Some(SearchClause {
            negated,
            term: SearchTerm::Text(SearchText::word(word)),
        }),
        rest,
    )
}

fn is_operator(name: &str) -> bool {
    [
        "from", "to", "subject", "has", "is", "in", "before", "after",
    ]
    .iter()
    .any(|operator| operator.eq_ignore_ascii_case(name))
}

/// Builds the term for a known operator, or `None` when the value is not
/// one the operator understands (the clause then stays plain text).
fn operator_term(name: &str, value: &str, phrase: bool) -> Option<SearchTerm> {
    let text = SearchText {
        value: value.to_owned(),
        phrase,
    };
    match name.to_ascii_lowercase().as_str() {
        "from" => Some(SearchTerm::From(text)),
        "to" => Some(SearchTerm::To(text)),
        "subject" => Some(SearchTerm::Subject(text)),
        "in" => Some(SearchTerm::Mailbox(text.value)),
        "has" => match text.value.to_ascii_lowercase().as_str() {
            "attachment" | "attachments" => Some(SearchTerm::HasAttachment),
            _ => None,
        },
        "is" => match text.value.to_ascii_lowercase().as_str() {
            "unread" => Some(SearchTerm::Unread),
            "read" => Some(SearchTerm::Read),
            "flagged" | "starred" => Some(SearchTerm::Flagged),
            _ => None,
        },
        "before" => DateSpec::parse(&text.value).map(SearchTerm::Before),
        "after" => DateSpec::parse(&text.value).map(SearchTerm::After),
        _ => None,
    }
}

/// Takes a `"quoted"` string from the start of `input`. An unterminated
/// quote runs to the end of the input.
fn take_quoted(input: &str) -> (String, &str) {
    let inner = &input[1..];
    match inner.find('"') {
        Some(end) => (inner[..end].trim().to_owned(), &inner[end + 1..]),
        None => (inner.trim().to_owned(), ""),
    }
}

/// Takes everything up to the next whitespace.
fn take_word(input: &str) -> (&str, &str) {
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    (&input[..end], &input[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> SearchTerm {
        SearchTerm::Text(SearchText::word(value))
    }

    fn phrase(value: &str) -> SearchText {
        SearchText {
            value: value.to_owned(),
            phrase: true,
        }
    }

    fn positive(term: SearchTerm) -> SearchClause {
        SearchClause {
            negated: false,
            term,
        }
    }

    fn negative(term: SearchTerm) -> SearchClause {
        SearchClause {
            negated: true,
            term,
        }
    }

    fn day(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("valid date")
    }

    #[test]
    fn plain_words_become_text_clauses() {
        assert_eq!(
            SearchQuery::parse("  quarterly   report "),
            SearchQuery {
                clauses: vec![positive(text("quarterly")), positive(text("report"))],
            }
        );
        assert!(SearchQuery::parse("   ").is_empty());
    }

    #[test]
    fn quoted_phrases_keep_their_words_together() {
        assert_eq!(
            SearchQuery::parse("\"budget review\" -\"out of office\"").clauses,
            vec![
                positive(SearchTerm::Text(phrase("budget review"))),
                negative(SearchTerm::Text(phrase("out of office"))),
            ]
        );
        // An unterminated quote runs to the end; empty quotes are dropped.
        assert_eq!(
            SearchQuery::parse("\"\" \"open ended").clauses,
            vec![positive(SearchTerm::Text(phrase("open ended")))]
        );
    }

    #[test]
    fn field_operators_take_words_or_quoted_values() {
        assert_eq!(
            SearchQuery::parse("from:alice To:\"Bob Jones\" SUBJECT:invoice").clauses,
            vec![
                positive(SearchTerm::From(SearchText::word("alice"))),
                positive(SearchTerm::To(phrase("Bob Jones"))),
                positive(SearchTerm::Subject(SearchText::word("invoice"))),
            ]
        );
    }

    #[test]
    fn state_and_mailbox_operators() {
        assert_eq!(
            SearchQuery::parse("has:attachment is:unread is:read is:starred in:\"Project X\"")
                .clauses,
            vec![
                positive(SearchTerm::HasAttachment),
                positive(SearchTerm::Unread),
                positive(SearchTerm::Read),
                positive(SearchTerm::Flagged),
                positive(SearchTerm::Mailbox("Project X".to_owned())),
            ]
        );
    }

    #[test]
    fn negation_applies_to_any_clause() {
        assert_eq!(
            SearchQuery::parse("-spam -from:bob -is:flagged - lone").clauses,
            vec![
                negative(text("spam")),
                negative(SearchTerm::From(SearchText::word("bob"))),
                negative(SearchTerm::Flagged),
                positive(text("lone")),
            ]
        );
        // A hyphen inside a word is just part of the word.
        assert_eq!(
            SearchQuery::parse("e-mail").clauses,
            vec![positive(text("e-mail"))]
        );
    }

    #[test]
    fn dates_parse_absolute_and_relative_forms() {
        assert_eq!(
            SearchQuery::parse("after:2026-01-31 before:7d before:2W after:3m after:1y").clauses,
            vec![
                positive(SearchTerm::After(DateSpec::Day(day(2026, 1, 31)))),
                positive(SearchTerm::Before(DateSpec::Ago {
                    amount: 7,
                    unit: DateUnit::Days,
                })),
                positive(SearchTerm::Before(DateSpec::Ago {
                    amount: 2,
                    unit: DateUnit::Weeks,
                })),
                positive(SearchTerm::After(DateSpec::Ago {
                    amount: 3,
                    unit: DateUnit::Months,
                })),
                positive(SearchTerm::After(DateSpec::Ago {
                    amount: 1,
                    unit: DateUnit::Years,
                })),
            ]
        );
    }

    #[test]
    fn relative_dates_resolve_against_today() {
        let today = day(2026, 3, 31);
        let ago = |amount, unit| DateSpec::Ago { amount, unit }.resolve(today);
        assert_eq!(ago(7, DateUnit::Days), Some(day(2026, 3, 24)));
        assert_eq!(ago(2, DateUnit::Weeks), Some(day(2026, 3, 17)));
        // Month arithmetic clamps to the end of shorter months.
        assert_eq!(ago(1, DateUnit::Months), Some(day(2026, 2, 28)));
        assert_eq!(ago(2, DateUnit::Years), Some(day(2024, 3, 31)));
        assert_eq!(ago(0, DateUnit::Days), Some(today));
        assert_eq!(ago(u32::MAX, DateUnit::Years), None);
        assert_eq!(
            DateSpec::Day(day(2020, 5, 1)).resolve(today),
            Some(day(2020, 5, 1))
        );
    }

    #[test]
    fn unrecognised_operators_stay_plain_text() {
        assert_eq!(
            SearchQuery::parse("foo:bar is:important has:link before:soon after:2026-13-01 10:30")
                .clauses,
            vec![
                positive(text("foo:bar")),
                positive(text("is:important")),
                positive(text("has:link")),
                positive(text("before:soon")),
                positive(text("after:2026-13-01")),
                positive(text("10:30")),
            ]
        );
        // `before:d` has no amount, so it is text too.
        assert_eq!(
            SearchQuery::parse("before:d").clauses,
            vec![positive(text("before:d"))]
        );
    }

    #[test]
    fn empty_operator_values_are_dropped() {
        assert_eq!(SearchQuery::parse("from: to:\"\"").clauses.len(), 1);
        assert_eq!(
            SearchQuery::parse("from: alice").clauses,
            // `from:` followed by a space is a word, not an operator.
            vec![positive(text("from:")), positive(text("alice"))]
        );
        assert!(SearchQuery::parse("to:\"\"").is_empty());
    }

    #[test]
    fn only_positive_free_text_needs_bodies() {
        assert!(SearchQuery::parse("is:unread report").needs_message_bodies());
        assert!(SearchQuery::parse("\"exact words\"").needs_message_bodies());
        assert!(!SearchQuery::parse("from:alice subject:x is:unread").needs_message_bodies());
        assert!(!SearchQuery::parse("-report !!").needs_message_bodies());
    }

    #[test]
    fn mailboxes_match_by_name_or_role() {
        assert!(mailbox_matches("inbox", "INBOX", None));
        assert!(mailbox_matches("Spam", "Bulk", Some("junk")));
        assert!(mailbox_matches("archive", "All Mail", Some("archive")));
        assert!(!mailbox_matches("project", "Projects", None));
    }

    #[test]
    fn day_starts_follow_the_zone() {
        let day = NaiveDate::from_ymd_opt(2026, 6, 1).expect("valid date");
        let plus_one = chrono::FixedOffset::east_opt(3600).expect("valid offset");
        assert_eq!(day_start_unix(day, &chrono::Utc), Some(1_780_272_000));
        assert_eq!(day_start_unix(day, &plus_one), Some(1_780_268_400));
    }

    #[test]
    fn text_terms_are_reported() {
        assert!(SearchQuery::parse("is:unread report").has_text_terms());
        assert!(SearchQuery::parse("from:alice").has_text_terms());
        assert!(!SearchQuery::parse("is:unread in:inbox after:7d").has_text_terms());
    }

    #[test]
    fn the_first_positive_in_clause_names_the_folder() {
        let clause = |query: &str| {
            SearchQuery::parse(query)
                .mailbox_clause()
                .map(str::to_owned)
        };
        assert_eq!(clause("report"), None);
        assert_eq!(clause("report -in:spam"), None);
        assert_eq!(
            clause("-in:spam in:\"Project X\" in:inbox"),
            Some("Project X".to_owned())
        );
        assert_eq!(clause("IN:archive"), Some("archive".to_owned()));
    }

    #[test]
    fn punctuation_only_text_is_not_searchable() {
        assert!(!SearchText::word("!!").is_searchable());
        assert!(SearchText::word("ünïcode").is_searchable());
        assert!(SearchText::word("42").is_searchable());
    }

    #[test]
    fn unicode_input_never_splits_inside_a_character() {
        // Byte slicing around quotes, colons and whitespace must stay on
        // character boundaries.
        let query = SearchQuery::parse("from:\"Zoë\u{2003}Ünal\" 日本:語 \u{3000}-é");
        assert_eq!(query.clauses.len(), 3);
        assert_eq!(
            query.clauses[0],
            positive(SearchTerm::From(phrase("Zoë\u{2003}Ünal")))
        );
        assert_eq!(query.clauses[1], positive(text("日本:語")));
        assert_eq!(query.clauses[2], negative(text("é")));
    }

    #[test]
    fn arbitrary_input_never_panics() {
        // A deterministic generator over the characters the parser treats
        // specially, plus multi-byte and whitespace characters.
        const ALPHABET: &[char] = &[
            '-', '"', ':', ' ', '\t', 'a', 'Z', '1', 'é', '日', '\u{3000}', '*', '(', ')', 'f',
            'r', 'o', 'm', 'i', 's', 'n',
        ];
        let mut state: u64 = 0x5eed;
        for _ in 0..20_000 {
            let mut input = String::new();
            for _ in 0..(state % 24) {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                input.push(ALPHABET[(state >> 33) as usize % ALPHABET.len()]);
            }
            state = state.wrapping_add(1);
            let query = SearchQuery::parse(&input);
            for clause in &query.clauses {
                if let SearchTerm::Text(text) = &clause.term {
                    assert!(!text.value.is_empty(), "{input:?}");
                }
            }
        }
    }
}
