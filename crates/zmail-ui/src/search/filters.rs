//! The search filter chips (UI plan Phase 7 item 3): from:, has:attachment,
//! unread and a date range.
//!
//! The chips have no state of their own. What they show is read from the
//! query ([`SearchFilters::of`]), and choosing one edits the query text
//! clause by clause through the operator parser ([`apply_filter`]), so the
//! text in the search field stays the one source of truth: typing
//! `is:unread` lights the Unread chip, and the chip adds or removes exactly
//! that clause.

use zmail_core::search::{SearchClause, SearchQuery, SearchTerm, with_clause, without_clauses};

/// A chip's edit to the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchFilter {
    /// Removes the `from:` clauses, or starts one for the user to finish.
    From,
    /// Toggles `has:attachment`.
    HasAttachment,
    /// Toggles `is:unread`.
    Unread,
    /// Replaces the date clauses with a preset range.
    Date(DateRange),
}

/// The date chip's preset ranges. Other ranges are typed as `after:` and
/// `before:` clauses, and the chip shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateRange {
    AnyTime,
    LastWeek,
    LastMonth,
    LastSixMonths,
    LastYear,
    OlderThanAYear,
}

impl DateRange {
    /// Every range, in menu order.
    pub(crate) const ALL: [Self; 6] = [
        Self::AnyTime,
        Self::LastWeek,
        Self::LastMonth,
        Self::LastSixMonths,
        Self::LastYear,
        Self::OlderThanAYear,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AnyTime => "Any time",
            Self::LastWeek => "Last 7 days",
            Self::LastMonth => "Last 30 days",
            Self::LastSixMonths => "Last 6 months",
            Self::LastYear => "Last year",
            Self::OlderThanAYear => "Older than a year",
        }
    }

    /// The clause the range adds, if any.
    fn clause_text(self) -> Option<&'static str> {
        match self {
            Self::AnyTime => None,
            Self::LastWeek => Some("after:7d"),
            Self::LastMonth => Some("after:30d"),
            Self::LastSixMonths => Some("after:6m"),
            Self::LastYear => Some("after:1y"),
            Self::OlderThanAYear => Some("before:1y"),
        }
    }

    /// The range a query's date clauses amount to, when they are exactly
    /// one preset.
    fn of(clauses: &[&SearchClause]) -> Option<Self> {
        let [clause] = clauses else {
            return (clauses.is_empty()).then_some(Self::AnyTime);
        };
        Self::ALL.into_iter().find(|range| {
            range
                .clause_text()
                .is_some_and(|text| SearchQuery::parse(text).clauses.first() == Some(*clause))
        })
    }
}

/// What the chips show for a query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SearchFilters {
    /// The `from:` values, joined, when the query has any.
    pub(crate) from: Option<String>,
    pub(crate) has_attachment: bool,
    pub(crate) unread: bool,
    /// The preset the date clauses match, if they match one.
    pub(crate) date_range: Option<DateRange>,
    /// The date clauses as typed, when there are any.
    pub(crate) date_text: Option<String>,
}

impl SearchFilters {
    /// Reads the chips' state from the query text.
    pub(crate) fn of(query: &str) -> Self {
        let spans = SearchQuery::parse_spans(query);
        let positive = |term: &SearchTerm| {
            spans
                .iter()
                .any(|(_, clause)| !clause.negated && clause.term == *term)
        };
        let from = spans
            .iter()
            .filter_map(|(_, clause)| match &clause.term {
                SearchTerm::From(text) if !clause.negated => Some(text.value.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let dates = spans
            .iter()
            .filter(|(_, clause)| is_date(clause))
            .collect::<Vec<_>>();
        Self {
            from: (!from.is_empty()).then(|| from.join(", ")),
            has_attachment: positive(&SearchTerm::HasAttachment),
            unread: positive(&SearchTerm::Unread),
            date_range: DateRange::of(&dates.iter().map(|(_, clause)| clause).collect::<Vec<_>>()),
            date_text: (!dates.is_empty()).then(|| {
                dates
                    .iter()
                    .map(|(span, _)| &query[span.clone()])
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        }
    }

    /// The date chip's label: the preset, the typed range, or "Date".
    pub(crate) fn date_label(&self) -> String {
        match (self.date_range, &self.date_text) {
            (Some(range), Some(_)) => range.label().to_owned(),
            (_, Some(text)) => text.clone(),
            (_, None) => "Date".to_owned(),
        }
    }
}

/// `query` with `filter` applied. Only the clauses the chip is about
/// change; the rest of the text stays as typed.
pub(crate) fn apply_filter(query: &str, filter: SearchFilter) -> String {
    let filters = SearchFilters::of(query);
    let positive =
        |term: SearchTerm| move |clause: &SearchClause| !clause.negated && clause.term == term;
    match filter {
        SearchFilter::From if filters.from.is_some() => without_clauses(query, |clause| {
            !clause.negated && matches!(clause.term, SearchTerm::From(_))
        }),
        // The user types the sender after it.
        SearchFilter::From => with_clause(query, "from:"),
        SearchFilter::HasAttachment if filters.has_attachment => {
            without_clauses(query, positive(SearchTerm::HasAttachment))
        }
        SearchFilter::HasAttachment => with_clause(
            &without_clauses(query, |clause| {
                clause.negated && clause.term == SearchTerm::HasAttachment
            }),
            "has:attachment",
        ),
        SearchFilter::Unread if filters.unread => {
            without_clauses(query, positive(SearchTerm::Unread))
        }
        // `is:read` would contradict it.
        SearchFilter::Unread => with_clause(
            &without_clauses(query, |clause| {
                matches!(clause.term, SearchTerm::Unread | SearchTerm::Read)
            }),
            "is:unread",
        ),
        SearchFilter::Date(range) => {
            let rest = without_clauses(query, is_date);
            match range.clause_text() {
                Some(clause) => with_clause(&rest, clause),
                None => rest,
            }
        }
    }
}

fn is_date(clause: &SearchClause) -> bool {
    matches!(clause.term, SearchTerm::Before(_) | SearchTerm::After(_))
}

#[cfg(test)]
mod tests {
    use zmail_core::search::{DateSpec, DateUnit};

    use super::*;

    /// The date a preset's clause parses to, so the presets and the
    /// parser cannot drift apart unnoticed.
    fn preset_spec(range: DateRange) -> Option<DateSpec> {
        let clause = SearchQuery::parse(range.clause_text()?).clauses.pop()?;
        match clause.term {
            SearchTerm::Before(spec) | SearchTerm::After(spec) => Some(spec),
            _ => None,
        }
    }

    #[test]
    fn the_chips_read_the_query() {
        let filters = SearchFilters::of("budget from:alice -is:unread has:attachment after:7d");
        assert_eq!(filters.from.as_deref(), Some("alice"));
        assert!(filters.has_attachment);
        assert!(!filters.unread, "a negated clause is not the chip");
        assert_eq!(filters.date_range, Some(DateRange::LastWeek));
        assert_eq!(filters.date_label(), "Last 7 days");

        let typed = SearchFilters::of("after:2026-01-01 before:2026-02-01");
        assert_eq!(typed.date_range, None);
        assert_eq!(typed.date_label(), "after:2026-01-01 before:2026-02-01");

        let none = SearchFilters::of("budget");
        assert_eq!(
            none,
            SearchFilters {
                date_range: Some(DateRange::AnyTime),
                ..SearchFilters::default()
            }
        );
        assert_eq!(none.date_label(), "Date");
    }

    #[test]
    fn toggles_add_and_remove_exactly_their_clause() {
        let unread = apply_filter("budget  report", SearchFilter::Unread);
        assert_eq!(unread, "budget  report is:unread");
        assert_eq!(
            apply_filter(&unread, SearchFilter::Unread),
            "budget  report"
        );
        // Turning Unread on drops a contradicting is:read.
        assert_eq!(
            apply_filter("is:read budget", SearchFilter::Unread),
            "budget is:unread"
        );

        let attached = apply_filter("-has:attachment x", SearchFilter::HasAttachment);
        assert_eq!(attached, "x has:attachment");
        assert_eq!(apply_filter(&attached, SearchFilter::HasAttachment), "x");

        // From starts a clause for the user to finish, or removes them.
        assert_eq!(apply_filter("x", SearchFilter::From), "x from:");
        assert_eq!(
            apply_filter("from:alice x from:\"Bob J\"", SearchFilter::From),
            "x"
        );
    }

    #[test]
    fn a_date_range_replaces_the_date_clauses() {
        let query = "x after:2026-01-01 -before:3d";
        assert_eq!(
            apply_filter(query, SearchFilter::Date(DateRange::LastMonth)),
            "x after:30d"
        );
        assert_eq!(
            apply_filter("x after:30d", SearchFilter::Date(DateRange::AnyTime)),
            "x"
        );
        assert_eq!(
            apply_filter("", SearchFilter::Date(DateRange::OlderThanAYear)),
            "before:1y"
        );
    }

    #[test]
    fn every_preset_parses_as_a_date() {
        assert_eq!(preset_spec(DateRange::AnyTime), None);
        assert_eq!(
            preset_spec(DateRange::LastSixMonths),
            Some(DateSpec::Ago {
                amount: 6,
                unit: DateUnit::Months,
            })
        );
        for range in DateRange::ALL.into_iter().skip(1) {
            assert!(preset_spec(range).is_some(), "{range:?}");
            let query = apply_filter("", SearchFilter::Date(range));
            assert_eq!(SearchFilters::of(&query).date_range, Some(range));
        }
    }
}
