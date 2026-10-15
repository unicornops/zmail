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
}
