//! Human-readable dates for message lists and the reader.
//!
//! Timestamps are stored as RFC 3339 strings (`2026-10-15T10:42:00+00:00`),
//! or as SQLite `CURRENT_TIMESTAMP` values (`2026-10-15 10:42:00`, UTC) for
//! a few local-only rows. [`format_list_date`] turns them into the short,
//! relative labels most mail clients show:
//!
//! | When (in the viewer's time zone) | Label |
//! |---|---|
//! | Today | `10:42` (or `10:42 AM` on a 12-hour clock) |
//! | Yesterday | `Yesterday` |
//! | 2–6 days ago | `Mon` |
//! | Earlier this year | `12 Oct` |
//! | Before this year | `12 Oct 2024` |
//!
//! Timestamps in the future (clock skew) show the time when they fall on
//! today's date and the calendar date otherwise.
//!
//! [`format_full_date`] gives the complete timestamp for tooltips and
//! quoted replies. Labels are English, like the rest of the shell, and
//! only the 12/24-hour clock follows the locale (see [`HourCycle`]).

use chrono::{DateTime, Datelike, FixedOffset, NaiveDateTime, TimeZone};

/// Whether times use a 12-hour (`3:05 PM`) or 24-hour (`15:05`) clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HourCycle {
    /// 12-hour clock with an AM/PM marker.
    H12,
    /// 24-hour clock.
    #[default]
    H24,
}

impl HourCycle {
    /// Reads the clock preference from the POSIX locale environment.
    ///
    /// Checks `LC_ALL`, then `LC_TIME`, then `LANG`, following POSIX
    /// precedence, and applies [`HourCycle::for_locale`]. When none is set
    /// (common for apps launched from the macOS Finder or on Windows),
    /// this falls back to the 24-hour clock. A Settings override is planned
    /// for the Settings window (UI plan, Phase 6).
    pub fn from_env() -> Self {
        ["LC_ALL", "LC_TIME", "LANG"]
            .iter()
            .filter_map(|name| std::env::var(name).ok())
            .find(|value| !value.trim().is_empty())
            .map(|value| Self::for_locale(&value))
            .unwrap_or_default()
    }

    /// The conventional clock for a POSIX locale name such as
    /// `en_US.UTF-8` or `de_DE@euro`.
    ///
    /// Regions that conventionally use a 12-hour clock (per CLDR's
    /// preferred hour cycle) get [`HourCycle::H12`]; everything else,
    /// including `C` and `POSIX`, gets [`HourCycle::H24`].
    pub fn for_locale(locale: &str) -> Self {
        // Strip the codeset and modifier: en_US.UTF-8@x -> en_US.
        let base = locale.split(['.', '@']).next().unwrap_or_default().trim();
        let region = base
            .split(['_', '-'])
            .nth(1)
            .unwrap_or_default()
            .to_ascii_uppercase();
        const TWELVE_HOUR_REGIONS: &[&str] = &[
            "US", "CA", "AU", "NZ", "PH", "IN", "PK", "BD", "EG", "SA", "AE", "JO", "KW", "QA",
            "OM", "BH", "IQ", "SY", "LB", "YE", "SD", "LY", "MY", "CO", "MX", "SV", "HN", "NI",
            "VE", "PR", "TW", "KR",
        ];
        if TWELVE_HOUR_REGIONS.contains(&region.as_str()) {
            Self::H12
        } else {
            Self::H24
        }
    }
}

/// Parses a stored timestamp.
///
/// Accepts RFC 3339 and SQLite's `YYYY-MM-DD HH:MM:SS` (read as UTC).
/// Returns `None` for anything else, so callers can fall back to showing
/// the raw value.
pub fn parse_timestamp(value: &str) -> Option<DateTime<FixedOffset>> {
    let value = value.trim();
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed);
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc().fixed_offset())
}

/// Formats a stored timestamp as a short relative label for a message
/// list (see the module docs for the tiers).
///
/// `now` sets both the reference time and the viewer's time zone; the
/// shell passes `chrono::Local::now()`. Unparseable input is returned
/// unchanged.
pub fn format_list_date<Tz: TimeZone>(
    timestamp: &str,
    now: &DateTime<Tz>,
    hour_cycle: HourCycle,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(parsed) = parse_timestamp(timestamp) else {
        return timestamp.to_owned();
    };
    let local = parsed.with_timezone(&now.timezone());
    let date = local.date_naive();
    let today = now.date_naive();

    if date == today {
        return format_time(&local, hour_cycle);
    }
    if date > today {
        return format_day_month(&local, local.year() != now.year());
    }
    match (today - date).num_days() {
        1 => "Yesterday".to_owned(),
        2..=6 => local.format("%a").to_string(),
        _ => format_day_month(&local, local.year() != now.year()),
    }
}

/// Formats a stored timestamp in full, for tooltips and quoted replies:
/// `Thu 15 Oct 2026, 10:42`.
///
/// The timestamp is shown in `zone`'s time zone. Unparseable input is
/// returned unchanged.
pub fn format_full_date<Tz: TimeZone>(timestamp: &str, zone: &Tz, hour_cycle: HourCycle) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(parsed) = parse_timestamp(timestamp) else {
        return timestamp.to_owned();
    };
    let local = parsed.with_timezone(zone);
    format!(
        "{}, {}",
        local.format("%a %-d %b %Y"),
        format_time(&local, hour_cycle)
    )
}

/// The first instant of the day after `now`, in `now`'s time zone; the
/// shell re-renders then so "Today" labels roll over to "Yesterday".
///
/// Falls back to 24 hours from `now` if local midnight does not exist (a
/// DST jump at midnight).
pub fn next_local_midnight<Tz: TimeZone>(now: &DateTime<Tz>) -> DateTime<Tz> {
    let zone = now.timezone();
    now.date_naive()
        .succ_opt()
        .and_then(|tomorrow| tomorrow.and_hms_opt(0, 0, 0))
        .and_then(|midnight| zone.from_local_datetime(&midnight).earliest())
        .unwrap_or_else(|| now.clone() + chrono::Duration::hours(24))
}

/// Formats seconds since the Unix epoch as a UTC RFC 3339 timestamp,
/// `2026-10-15T10:42:00Z`, the form the local store writes for its own
/// rows. Strings in this form sort chronologically.
pub fn utc_timestamp(seconds_since_epoch: u64) -> String {
    i64::try_from(seconds_since_epoch)
        .ok()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn format_time<Tz: TimeZone>(local: &DateTime<Tz>, hour_cycle: HourCycle) -> String
where
    Tz::Offset: std::fmt::Display,
{
    match hour_cycle {
        HourCycle::H24 => local.format("%H:%M").to_string(),
        HourCycle::H12 => local.format("%-I:%M %p").to_string(),
    }
}

fn format_day_month<Tz: TimeZone>(local: &DateTime<Tz>, with_year: bool) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if with_year {
        local.format("%-d %b %Y").to_string()
    } else {
        local.format("%-d %b").to_string()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, FixedOffset, TimeZone, Utc};

    use super::{
        HourCycle, format_full_date, format_list_date, next_local_midnight, parse_timestamp,
    };

    fn utc(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .expect("valid test timestamp")
            .with_timezone(&Utc)
    }

    fn zone(hours: i32) -> FixedOffset {
        FixedOffset::east_opt(hours * 3600).expect("valid offset")
    }

    #[test]
    fn today_shows_the_time_on_either_clock() {
        let now = utc("2026-10-15T16:30:00Z");
        let ts = "2026-10-15T10:42:00+00:00";
        assert_eq!(format_list_date(ts, &now, HourCycle::H24), "10:42");
        assert_eq!(format_list_date(ts, &now, HourCycle::H12), "10:42 AM");
        let afternoon = "2026-10-15T15:05:00Z";
        assert_eq!(format_list_date(afternoon, &now, HourCycle::H24), "15:05");
        assert_eq!(format_list_date(afternoon, &now, HourCycle::H12), "3:05 PM");
        let midnight = "2026-10-15T00:00:00Z";
        assert_eq!(format_list_date(midnight, &now, HourCycle::H24), "00:00");
        assert_eq!(format_list_date(midnight, &now, HourCycle::H12), "12:00 AM");
    }

    #[test]
    fn yesterday_is_named() {
        let now = utc("2026-10-15T00:05:00Z");
        assert_eq!(
            format_list_date("2026-10-14T23:59:00Z", &now, HourCycle::H24),
            "Yesterday"
        );
        assert_eq!(
            format_list_date("2026-10-14T00:00:00Z", &now, HourCycle::H24),
            "Yesterday"
        );
    }

    #[test]
    fn the_last_week_shows_the_weekday() {
        // 15 Oct 2026 is a Thursday.
        let now = utc("2026-10-15T12:00:00Z");
        assert_eq!(
            format_list_date("2026-10-13T09:00:00Z", &now, HourCycle::H24),
            "Tue"
        );
        assert_eq!(
            format_list_date("2026-10-09T09:00:00Z", &now, HourCycle::H24),
            "Fri"
        );
        // Seven days back is the same weekday, so it gets the date.
        assert_eq!(
            format_list_date("2026-10-08T09:00:00Z", &now, HourCycle::H24),
            "8 Oct"
        );
    }

    #[test]
    fn this_year_shows_day_and_month() {
        let now = utc("2026-10-15T12:00:00Z");
        assert_eq!(
            format_list_date("2026-01-02T09:00:00Z", &now, HourCycle::H24),
            "2 Jan"
        );
        assert_eq!(
            format_list_date("2026-09-12T09:00:00Z", &now, HourCycle::H24),
            "12 Sep"
        );
    }

    #[test]
    fn older_mail_includes_the_year() {
        let now = utc("2026-10-15T12:00:00Z");
        assert_eq!(
            format_list_date("2024-10-12T09:00:00Z", &now, HourCycle::H24),
            "12 Oct 2024"
        );
    }

    #[test]
    fn year_boundary_uses_relative_tiers_then_the_year() {
        // 1 Jan 2027 is a Friday.
        let now = utc("2027-01-01T09:00:00Z");
        assert_eq!(
            format_list_date("2026-12-31T22:00:00Z", &now, HourCycle::H24),
            "Yesterday"
        );
        assert_eq!(
            format_list_date("2026-12-28T22:00:00Z", &now, HourCycle::H24),
            "Mon"
        );
        assert_eq!(
            format_list_date("2026-12-20T22:00:00Z", &now, HourCycle::H24),
            "20 Dec 2026"
        );
    }

    #[test]
    fn dates_are_judged_in_the_viewers_time_zone() {
        // 23:30 UTC on the 14th is already the 15th in UTC+2...
        let ts = "2026-10-14T23:30:00Z";
        let now_plus_two = zone(2)
            .with_ymd_and_hms(2026, 10, 15, 9, 0, 0)
            .single()
            .expect("valid time");
        assert_eq!(format_list_date(ts, &now_plus_two, HourCycle::H24), "01:30");
        // ...but still the 14th in UTC-5, a day before that viewer's today.
        let now_minus_five = zone(-5)
            .with_ymd_and_hms(2026, 10, 15, 9, 0, 0)
            .single()
            .expect("valid time");
        assert_eq!(
            format_list_date(ts, &now_minus_five, HourCycle::H24),
            "Yesterday"
        );
        // A message with its own offset converts to the viewer's zone.
        assert_eq!(
            format_list_date("2026-10-15T08:00:00-07:00", &now_plus_two, HourCycle::H24),
            "17:00"
        );
    }

    #[test]
    fn year_boundary_depends_on_the_time_zone() {
        // 23:00 UTC on 31 Dec is already 1 Jan in UTC+3.
        let ts = "2026-12-31T23:00:00Z";
        let now = zone(3)
            .with_ymd_and_hms(2027, 1, 1, 12, 0, 0)
            .single()
            .expect("valid time");
        assert_eq!(format_list_date(ts, &now, HourCycle::H24), "02:00");
        let now_utc = utc("2027-01-01T12:00:00Z");
        assert_eq!(format_list_date(ts, &now_utc, HourCycle::H24), "Yesterday");
    }

    #[test]
    fn future_timestamps_show_time_or_date() {
        let now = utc("2026-10-15T12:00:00Z");
        assert_eq!(
            format_list_date("2026-10-15T13:00:00Z", &now, HourCycle::H24),
            "13:00"
        );
        assert_eq!(
            format_list_date("2026-10-16T13:00:00Z", &now, HourCycle::H24),
            "16 Oct"
        );
        assert_eq!(
            format_list_date("2027-02-01T13:00:00Z", &now, HourCycle::H24),
            "1 Feb 2027"
        );
    }

    #[test]
    fn sqlite_timestamps_are_read_as_utc() {
        let parsed = parse_timestamp("2026-10-15 10:42:00").expect("sqlite format parses");
        assert_eq!(parsed, utc("2026-10-15T10:42:00Z"));
        let now = utc("2026-10-15T12:00:00Z");
        assert_eq!(
            format_list_date("2026-10-15 10:42:00", &now, HourCycle::H24),
            "10:42"
        );
    }

    #[test]
    fn unparseable_timestamps_pass_through() {
        let now = utc("2026-10-15T12:00:00Z");
        assert_eq!(
            format_list_date("not a date", &now, HourCycle::H24),
            "not a date"
        );
        assert_eq!(format_full_date("", &Utc, HourCycle::H24), "");
    }

    #[test]
    fn full_date_for_tooltips() {
        let ts = "2026-10-15T15:05:00Z";
        assert_eq!(
            format_full_date(ts, &Utc, HourCycle::H24),
            "Thu 15 Oct 2026, 15:05"
        );
        assert_eq!(
            format_full_date(ts, &zone(2), HourCycle::H12),
            "Thu 15 Oct 2026, 5:05 PM"
        );
    }

    #[test]
    fn next_midnight_is_the_start_of_tomorrow() {
        let now = zone(1)
            .with_ymd_and_hms(2026, 12, 31, 23, 59, 30)
            .single()
            .expect("valid time");
        let midnight = next_local_midnight(&now);
        assert_eq!(midnight.to_rfc3339(), "2027-01-01T00:00:00+01:00");
        assert_eq!((midnight - now).num_seconds(), 30);
    }

    #[test]
    fn utc_timestamps_are_real_dates() {
        assert_eq!(super::utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_timestamp(1_791_888_120), "2026-10-13T10:42:00Z");
        // Lexical order matches time order across a month boundary.
        assert!(super::utc_timestamp(1_790_812_799) < super::utc_timestamp(1_790_812_800));
    }

    #[test]
    fn hour_cycle_follows_the_locale_region() {
        assert_eq!(HourCycle::for_locale("en_US.UTF-8"), HourCycle::H12);
        assert_eq!(HourCycle::for_locale("en_AU"), HourCycle::H12);
        assert_eq!(HourCycle::for_locale("en-IN"), HourCycle::H12);
        assert_eq!(HourCycle::for_locale("en_GB.UTF-8"), HourCycle::H24);
        assert_eq!(HourCycle::for_locale("en_IE.UTF-8@euro"), HourCycle::H24);
        assert_eq!(HourCycle::for_locale("de_DE.UTF-8"), HourCycle::H24);
        assert_eq!(HourCycle::for_locale("C"), HourCycle::H24);
        assert_eq!(HourCycle::for_locale("POSIX"), HourCycle::H24);
        assert_eq!(HourCycle::for_locale(""), HourCycle::H24);
    }

    // Characterisation (issue #489). The tables below pin what the stored
    // formats parsed to and what the labels read before the workspace moved
    // off chrono, so neither can drift with the time library. Only `clock`,
    // `stamp` and `midnight_after` know which library that is.

    /// `now` as the viewer's clock, in a zone fixed at its offset.
    fn clock(now: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(now).expect("valid test clock")
    }

    /// A parsed timestamp as whole seconds since the epoch and the
    /// nanoseconds after them. A leap second (`:60`) is the last second of
    /// its minute with a further full second of nanoseconds.
    fn stamp(value: &str) -> Option<(i64, u32)> {
        parse_timestamp(value).map(|at| (at.timestamp(), at.timestamp_subsec_nanos()))
    }

    fn midnight_after(now: &str) -> String {
        next_local_midnight(&clock(now)).to_rfc3339()
    }

    #[test]
    fn stored_timestamps_parse_as_before() {
        const AT: i64 = 1_792_060_920; // 2026-10-15T10:42:00Z
        let cases: &[(&str, Option<(i64, u32)>)] = &[
            // The shapes the adapters and the store write.
            ("2026-10-15T10:42:00+00:00", Some((AT, 0))),
            ("2026-10-15T10:42:00Z", Some((AT, 0))),
            ("2026-10-15 10:42:00", Some((AT, 0))),
            // Offsets.
            ("2026-10-15T16:12:00+05:30", Some((AT, 0))),
            ("2026-10-15T02:42:00-08:00", Some((AT, 0))),
            ("2026-10-15T10:42:00-00:00", Some((AT, 0))),
            ("2026-10-16T10:41:00+23:59", Some((AT, 0))),
            ("2026-10-14T10:43:00-23:59", Some((AT, 0))),
            ("2026-10-15T10:42:00\u{2212}00:00", Some((AT, 0))),
            // Case, the space separator and surrounding white space.
            ("2026-10-15t10:42:00z", Some((AT, 0))),
            ("2026-10-15 10:42:00Z", Some((AT, 0))),
            ("2026-10-15 10:42:00+00:00", Some((AT, 0))),
            ("  2026-10-15T10:42:00Z\n", Some((AT, 0))),
            ("\t2026-10-15 10:42:00 ", Some((AT, 0))),
            // Fractions of a second, cut to nanoseconds.
            ("2026-10-15T10:42:00.5Z", Some((AT, 500_000_000))),
            ("2026-10-15T10:42:00.000000001Z", Some((AT, 1))),
            ("2026-10-15T10:42:00.123456789987Z", Some((AT, 123_456_789))),
            (
                "2026-10-15T10:42:00.25+05:30",
                Some((AT - 19_800, 250_000_000)),
            ),
            // Leap seconds, in any minute.
            ("2026-12-31T23:59:60Z", Some((1_798_761_599, 1_000_000_000))),
            ("2026-10-15T10:42:60Z", Some((AT + 59, 1_000_000_000))),
            (
                "2026-10-15T10:42:60.25+01:00",
                Some((AT - 3_541, 1_250_000_000)),
            ),
            ("2026-10-15 10:42:60", Some((AT + 59, 1_000_000_000))),
            // The ends of the four-digit years, and a leap day.
            ("0000-01-01T00:00:00Z", Some((-62_167_219_200, 0))),
            ("0000-01-01T00:00:00+23:59", Some((-62_167_305_540, 0))),
            ("1969-12-31T23:59:59Z", Some((-1, 0))),
            ("1970-01-01T00:00:00Z", Some((0, 0))),
            ("2024-02-29T12:00:00Z", Some((1_709_208_000, 0))),
            ("9999-12-30T21:59:59Z", Some((253_402_207_199, 0))),
            // SQLite's form is read loosely: one-digit fields, any white
            // space, a signed year.
            ("2026-10-15  10:42:00", Some((AT, 0))),
            ("2026-10-15\t10:42:00", Some((AT, 0))),
            ("2026-1-5 1:2:3", Some((1_767_574_923, 0))),
            ("+2026-10-15 10:42:00", Some((AT, 0))),
            ("-0001-10-15 10:42:00", Some((-62_173_919_880, 0))),
            ("2026 - 10 - 15 10 : 42 : 00", None),
            ("2026-10-15 10:42:00.5", None),
            ("2026-10-15 10:42", None),
            ("2026-10-15T10:42:00", None),
            // Not timestamps.
            ("", None),
            ("not a date", None),
            ("2026-10-15", None),
            ("1792060920", None),
            ("2026-10-15T10:42Z", None),
            ("2026-10-15T10:42:00+0000", None),
            ("2026-10-15T10:42:00+00", None),
            ("2026-10-15T10:42:00 +00:00", None),
            ("2026-10-15T10:42:00 Z", None),
            ("2026-10-15T10:42:00Z[UTC]", None),
            ("2026-10-15T10:42:00ZZ", None),
            ("2026-10-15T10:42:00,5Z", None),
            ("2026-10-15T10:42:00.Z", None),
            ("20261015T104200Z", None),
            ("2026-1-05T10:42:00Z", None),
            ("02026-10-15T10:42:00Z", None),
            ("+2026-10-15T10:42:00Z", None),
            ("2026-10-15T10:42:00+5:30", None),
            ("Thu, 15 Oct 2026 10:42:00 +0000", None),
            // Out of range fields.
            ("2026-13-01T00:00:00Z", None),
            ("2026-00-10T00:00:00Z", None),
            ("2026-02-29T00:00:00Z", None),
            ("2026-04-31T00:00:00Z", None),
            ("2026-10-15T24:00:00Z", None),
            ("2026-10-15T10:60:00Z", None),
            ("2026-10-15T10:42:61Z", None),
            ("2026-10-15T10:42:00+24:00", None),
            ("2026-10-15T10:42:00+00:60", None),
            ("2026-10-15 24:00:00", None),
            ("2026-02-30 10:42:00", None),
        ];
        for (value, expected) in cases {
            assert_eq!(stamp(value), *expected, "{value:?}");
        }
    }

    /// Instants past what the replacement time library can hold: the last
    /// 26 hours of the year 9999 and, in SQLite's form, signed years of
    /// more than four digits.
    #[test]
    fn timestamps_at_the_far_end_of_the_calendar() {
        assert_eq!(stamp("9999-12-30T22:00:00Z"), Some((253_402_207_200, 0)));
        assert_eq!(stamp("9999-12-31T23:59:59Z"), Some((253_402_300_799, 0)));
        assert_eq!(
            stamp("9999-12-31T23:59:59-23:59"),
            Some((253_402_387_139, 0))
        );
        assert_eq!(stamp("+12026-10-15 10:42:00"), Some((317_361_580_920, 0)));
        assert_eq!(stamp("12026-10-15 10:42:00"), None);
    }

    #[test]
    fn list_and_full_dates_read_as_before() {
        use HourCycle::{H12, H24};
        // (timestamp, the viewer's clock, hour cycle, list label, full date)
        let cases: &[(&str, &str, HourCycle, &str, &str)] = &[
            // Today, on either clock.
            (
                "2026-10-15T10:42:00+00:00",
                "2026-10-15T16:30:00+00:00",
                H24,
                "10:42",
                "Thu 15 Oct 2026, 10:42",
            ),
            (
                "2026-10-15T10:42:00+00:00",
                "2026-10-15T16:30:00+00:00",
                H12,
                "10:42 AM",
                "Thu 15 Oct 2026, 10:42 AM",
            ),
            (
                "2026-10-15T00:05:00Z",
                "2026-10-15T16:30:00+00:00",
                H12,
                "12:05 AM",
                "Thu 15 Oct 2026, 12:05 AM",
            ),
            (
                "2026-10-15T12:00:00Z",
                "2026-10-15T16:30:00+00:00",
                H12,
                "12:00 PM",
                "Thu 15 Oct 2026, 12:00 PM",
            ),
            (
                "2026-10-15T13:07:00Z",
                "2026-10-15T16:30:00+00:00",
                H12,
                "1:07 PM",
                "Thu 15 Oct 2026, 1:07 PM",
            ),
            (
                "2026-10-15T09:07:00Z",
                "2026-10-15T16:30:00+00:00",
                H24,
                "09:07",
                "Thu 15 Oct 2026, 09:07",
            ),
            (
                "2026-10-15T23:59:59Z",
                "2026-10-15T16:30:00+00:00",
                H24,
                "23:59",
                "Thu 15 Oct 2026, 23:59",
            ),
            // Seconds, fractions and leap seconds do not show.
            (
                "2026-10-15T10:42:59.999Z",
                "2026-10-15T16:30:00+00:00",
                H24,
                "10:42",
                "Thu 15 Oct 2026, 10:42",
            ),
            (
                "2026-10-15T10:42:60Z",
                "2026-10-15T16:30:00+00:00",
                H24,
                "10:42",
                "Thu 15 Oct 2026, 10:42",
            ),
            (
                "2026-10-14T23:59:60Z",
                "2026-10-15T16:30:00+00:00",
                H24,
                "Yesterday",
                "Wed 14 Oct 2026, 23:59",
            ),
            // SQLite's form and surrounding white space.
            (
                "2026-10-15 10:42:00",
                "2026-10-15T16:30:00+00:00",
                H24,
                "10:42",
                "Thu 15 Oct 2026, 10:42",
            ),
            (
                " 2026-10-15T10:42:00Z ",
                "2026-10-15T16:30:00+00:00",
                H24,
                "10:42",
                "Thu 15 Oct 2026, 10:42",
            ),
            // The viewer's offset moves the time and the day.
            (
                "2026-10-15T10:42:00Z",
                "2026-10-15T22:00:00+05:30",
                H24,
                "16:12",
                "Thu 15 Oct 2026, 16:12",
            ),
            (
                "2026-10-15T10:42:00Z",
                "2026-10-15T22:00:00+05:45",
                H12,
                "4:27 PM",
                "Thu 15 Oct 2026, 4:27 PM",
            ),
            (
                "2026-10-15T10:42:00Z",
                "2026-10-15T08:00:00-08:00",
                H24,
                "02:42",
                "Thu 15 Oct 2026, 02:42",
            ),
            (
                "2026-10-15T01:42:00Z",
                "2026-10-15T08:00:00-08:00",
                H24,
                "Yesterday",
                "Wed 14 Oct 2026, 17:42",
            ),
            (
                "2026-10-15T22:42:00Z",
                "2026-10-16T09:00:00+02:00",
                H24,
                "00:42",
                "Fri 16 Oct 2026, 00:42",
            ),
            (
                "2026-10-15T08:00:00-07:00",
                "2026-10-15T20:00:00+02:00",
                H24,
                "17:00",
                "Thu 15 Oct 2026, 17:00",
            ),
            (
                "2026-10-15T10:42:00Z",
                "2026-10-16T00:30:00+14:00",
                H24,
                "00:42",
                "Fri 16 Oct 2026, 00:42",
            ),
            (
                "2026-10-15T10:42:00Z",
                "2026-10-14T23:30:00-12:00",
                H24,
                "22:42",
                "Wed 14 Oct 2026, 22:42",
            ),
            // Yesterday, the weekdays of the last week, then dates.
            (
                "2026-10-14T00:00:00Z",
                "2026-10-15T00:00:00+00:00",
                H24,
                "Yesterday",
                "Wed 14 Oct 2026, 00:00",
            ),
            (
                "2026-10-13T23:59:59Z",
                "2026-10-15T00:00:00+00:00",
                H24,
                "Tue",
                "Tue 13 Oct 2026, 23:59",
            ),
            (
                "2026-10-12T12:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "Mon",
                "Mon 12 Oct 2026, 12:00",
            ),
            (
                "2026-10-11T12:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "Sun",
                "Sun 11 Oct 2026, 12:00",
            ),
            (
                "2026-10-10T12:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "Sat",
                "Sat 10 Oct 2026, 12:00",
            ),
            (
                "2026-10-09T00:00:00Z",
                "2026-10-15T23:59:59+00:00",
                H24,
                "Fri",
                "Fri 9 Oct 2026, 00:00",
            ),
            (
                "2026-10-08T23:59:59Z",
                "2026-10-15T00:00:00+00:00",
                H24,
                "8 Oct",
                "Thu 8 Oct 2026, 23:59",
            ),
            (
                "2026-01-01T00:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "1 Jan",
                "Thu 1 Jan 2026, 00:00",
            ),
            (
                "2025-12-31T23:59:59Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "31 Dec 2025",
                "Wed 31 Dec 2025, 23:59",
            ),
            // Across a new year, a leap day and a month end.
            (
                "2026-12-31T22:00:00Z",
                "2027-01-01T09:00:00+00:00",
                H24,
                "Yesterday",
                "Thu 31 Dec 2026, 22:00",
            ),
            (
                "2026-12-26T22:00:00Z",
                "2027-01-01T09:00:00+00:00",
                H24,
                "Sat",
                "Sat 26 Dec 2026, 22:00",
            ),
            (
                "2026-12-25T22:00:00Z",
                "2027-01-01T09:00:00+00:00",
                H24,
                "25 Dec 2026",
                "Fri 25 Dec 2026, 22:00",
            ),
            (
                "2026-12-31T23:00:00Z",
                "2027-01-01T12:00:00+03:00",
                H24,
                "02:00",
                "Fri 1 Jan 2027, 02:00",
            ),
            (
                "2024-02-29T12:00:00Z",
                "2024-03-01T12:00:00+00:00",
                H24,
                "Yesterday",
                "Thu 29 Feb 2024, 12:00",
            ),
            (
                "2024-02-28T12:00:00Z",
                "2024-03-05T12:00:00+00:00",
                H12,
                "Wed",
                "Wed 28 Feb 2024, 12:00 PM",
            ),
            // The future: today's time, or the date.
            (
                "2026-10-15T23:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "23:00",
                "Thu 15 Oct 2026, 23:00",
            ),
            (
                "2026-10-16T00:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "16 Oct",
                "Fri 16 Oct 2026, 00:00",
            ),
            (
                "2027-01-01T00:00:00Z",
                "2026-12-31T12:00:00+00:00",
                H24,
                "1 Jan 2027",
                "Fri 1 Jan 2027, 00:00",
            ),
            // Long ago.
            (
                "1970-01-01T00:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H12,
                "1 Jan 1970",
                "Thu 1 Jan 1970, 12:00 AM",
            ),
            (
                "1969-07-20T20:17:40Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "20 Jul 1969",
                "Sun 20 Jul 1969, 20:17",
            ),
            (
                "0999-03-05T04:05:06Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "5 Mar 0999",
                "Tue 5 Mar 0999, 04:05",
            ),
            (
                "0000-01-01T00:00:00Z",
                "2026-10-15T12:00:00+00:00",
                H24,
                "1 Jan 0000",
                "Sat 1 Jan 0000, 00:00",
            ),
            (
                "0000-01-01T00:00:00+01:00",
                "2026-10-15T12:00:00+00:00",
                H24,
                "31 Dec -0001",
                "Fri 31 Dec -0001, 23:00",
            ),
            // Not a timestamp: shown as it is.
            (
                "yesterday-ish",
                "2026-10-15T12:00:00+00:00",
                H24,
                "yesterday-ish",
                "yesterday-ish",
            ),
            (
                "2026-10-15T10:42",
                "2026-10-15T12:00:00+00:00",
                H24,
                "2026-10-15T10:42",
                "2026-10-15T10:42",
            ),
            ("", "2026-10-15T12:00:00+00:00", H12, "", ""),
        ];
        for (timestamp, now, hour_cycle, list, full) in cases {
            let now = clock(now);
            assert_eq!(
                format_list_date(timestamp, &now, *hour_cycle),
                *list,
                "list date of {timestamp:?} at {now}"
            );
            assert_eq!(
                format_full_date(timestamp, &now.timezone(), *hour_cycle),
                *full,
                "full date of {timestamp:?} at {now}"
            );
        }
    }

    #[test]
    fn the_next_midnight_is_found_as_before() {
        let cases = [
            ("2026-10-15T00:00:00+00:00", "2026-10-16T00:00:00+00:00"),
            ("2026-10-15T23:59:59+00:00", "2026-10-16T00:00:00+00:00"),
            ("2026-10-15T10:42:17.5+05:30", "2026-10-16T00:00:00+05:30"),
            ("2026-10-15T10:42:17-08:00", "2026-10-16T00:00:00-08:00"),
            ("2026-12-31T23:59:30+01:00", "2027-01-01T00:00:00+01:00"),
            ("2024-02-28T12:00:00-05:00", "2024-02-29T00:00:00-05:00"),
            ("2026-02-28T12:00:00+13:45", "2026-03-01T00:00:00+13:45"),
        ];
        for (now, expected) in cases {
            assert_eq!(midnight_after(now), expected, "{now}");
        }
    }

    #[test]
    fn utc_timestamps_are_written_as_before() {
        let cases: &[(u64, &str)] = &[
            (0, "1970-01-01T00:00:00Z"),
            (1, "1970-01-01T00:00:01Z"),
            (59, "1970-01-01T00:00:59Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_709_251_199, "2024-02-29T23:59:59Z"),
            (1_792_060_920, "2026-10-15T10:42:00Z"),
            (1_798_761_599, "2026-12-31T23:59:59Z"),
            (1_798_761_600, "2027-01-01T00:00:00Z"),
            (2_147_483_648, "2038-01-19T03:14:08Z"),
            (4_102_444_800, "2100-01-01T00:00:00Z"),
            (253_402_207_199, "9999-12-30T21:59:59Z"),
            // No such time: the epoch.
            (u64::MAX, "1970-01-01T00:00:00Z"),
            (i64::MAX as u64, "1970-01-01T00:00:00Z"),
        ];
        for (seconds, expected) in cases {
            assert_eq!(super::utc_timestamp(*seconds), *expected, "{seconds}");
        }
    }

    /// Past what the replacement time library can hold (see
    /// `timestamps_at_the_far_end_of_the_calendar`).
    #[test]
    fn utc_timestamps_at_the_far_end_of_the_calendar() {
        assert_eq!(
            super::utc_timestamp(253_402_207_200),
            "9999-12-30T22:00:00Z"
        );
        assert_eq!(
            super::utc_timestamp(253_402_300_799),
            "9999-12-31T23:59:59Z"
        );
        assert_eq!(
            super::utc_timestamp(253_402_300_800),
            "+10000-01-01T00:00:00Z"
        );
    }
}
