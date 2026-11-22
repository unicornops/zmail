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
//!
//! This module is also the workspace's one door to the time library
//! (`jiff`): the clock and the system time zone come from it, re-exported
//! here as [`ZonedDateTime`] and [`TimeZone`]. The text of timestamps does
//! not: [`Stamp`], [`Date`] and the parsers read and write stored and wire
//! formats with their own calendar arithmetic, so what an existing store
//! holds means the same whatever library tells the time.

mod calendar;
mod text;

use std::fmt;

use jiff::{
    SignedDuration, Timestamp,
    tz::{AmbiguousOffset, Offset},
};
pub use jiff::{ZonedDateTime, tz::TimeZone};

use calendar::{
    DAY, Fields, MAX_SECONDS, MIN_SECONDS, civil_from_days, days_from_civil, days_in_month,
    year_text,
};

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

/// An instant read from a stored or wire timestamp, or from the clock.
///
/// It keeps what the text said beyond the instant: a leap second (`:60`)
/// is the last second of its minute and stays a leap second, ordered after
/// that second and written back as `:60`. Instants run from the year
/// -262143 to 262142.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Stamp {
    seconds: i64,
    /// Nanoseconds after `seconds`; a further 1,000,000,000 in a leap
    /// second.
    nanos: u32,
}

impl Stamp {
    /// The Unix epoch, 1 January 1970 at midnight UTC.
    pub const EPOCH: Self = Self {
        seconds: 0,
        nanos: 0,
    };

    /// The current time.
    pub fn now() -> Self {
        Timestamp::now().into()
    }

    /// The instant `seconds` after the Unix epoch, when the calendar
    /// reaches that far.
    pub fn from_seconds(seconds: i64) -> Option<Self> {
        (MIN_SECONDS..=MAX_SECONDS)
            .contains(&seconds)
            .then_some(Self { seconds, nanos: 0 })
    }

    /// Whole seconds since the Unix epoch. A leap second counts as the
    /// second before it.
    pub fn seconds(self) -> i64 {
        self.seconds
    }

    /// Whole seconds from `earlier` to this instant, negative when this
    /// one is the earlier, rounded towards zero.
    pub fn seconds_since(self, earlier: Self) -> i64 {
        let nanos = |stamp: Self| {
            i128::from(stamp.seconds) * 1_000_000_000 + i128::from(stamp.nanos % 1_000_000_000)
        };
        ((nanos(self) - nanos(earlier)) / 1_000_000_000) as i64
    }

    /// `2026-10-15T10:42:00Z`: the UTC form the local store writes for its
    /// own rows, to the second. Strings in this form sort chronologically.
    pub fn format_utc(self) -> String {
        let utc = self.fields(0);
        format!(
            "{}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            year_text(utc.year),
            utc.month,
            utc.day,
            utc.hour,
            utc.minute,
            utc.second
        )
    }

    /// `2026-10-15T10:42:00+00:00`: RFC 3339 in UTC to the second, the
    /// form adapters write `received_at` in.
    pub fn format_rfc3339_utc(self) -> String {
        let utc = self.fields(0);
        format!(
            "{}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
            year_text(utc.year),
            utc.month,
            utc.day,
            utc.hour,
            utc.minute,
            utc.second
        )
    }

    /// `Thu, 15 Oct 2026 10:42:00 +0000`: the date of a mail header (Go's
    /// `time.RFC1123Z`), in UTC.
    pub fn format_rfc1123z(self) -> String {
        let utc = self.fields(0);
        format!(
            "{}, {:02} {} {} {:02}:{:02}:{:02} +0000",
            utc.weekday(),
            utc.day,
            utc.month_name(),
            year_text(utc.year),
            utc.hour,
            utc.minute,
            utc.second
        )
    }

    fn is_leap_second(self) -> bool {
        self.nanos >= 1_000_000_000
    }

    /// The date and time of day `offset` seconds east of UTC.
    fn fields(self, offset: i64) -> Fields {
        Fields::of(self.seconds + offset, self.is_leap_second())
    }

    /// The date and time of day in `zone`.
    fn fields_in(self, zone: &TimeZone) -> Fields {
        self.fields(offset_at(zone, self.seconds))
    }
}

impl From<Timestamp> for Stamp {
    fn from(timestamp: Timestamp) -> Self {
        // Split at the second below, so times before 1970 count their
        // nanoseconds forwards like every other.
        let nanos = timestamp.as_nanosecond();
        Self {
            seconds: nanos.div_euclid(1_000_000_000) as i64,
            nanos: nanos.rem_euclid(1_000_000_000) as u32,
        }
    }
}

impl From<&ZonedDateTime> for Stamp {
    fn from(zoned: &ZonedDateTime) -> Self {
        zoned.timestamp().into()
    }
}

/// A day of the proleptic Gregorian calendar, from the year -262143 to
/// 262142. Shown as `YYYY-MM-DD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// The day `year`-`month`-`day`, when there is one.
    pub fn new(year: i32, month: u8, day: u8) -> Option<Self> {
        calendar::is_date(i64::from(year), i64::from(month), i64::from(day)).then_some(Self {
            year,
            month,
            day,
        })
    }

    /// The date `now` shows in its time zone.
    pub fn of(now: &ZonedDateTime) -> Self {
        let date = now.date();
        Self {
            year: i32::from(date.year()),
            month: date.month() as u8,
            day: date.day() as u8,
        }
    }

    /// Parses `YYYY-MM-DD`. A number may be shorter, and the year signed.
    pub fn parse(value: &str) -> Option<Self> {
        let (year, month, day) = text::date(value)?;
        Self::new(i32::try_from(year).ok()?, month, day)
    }

    /// The day `days` days before this one, when the calendar reaches
    /// that far back.
    pub fn checked_sub_days(self, days: u64) -> Option<Self> {
        let days = self.day_number().checked_sub(i64::try_from(days).ok()?)?;
        if days < MIN_SECONDS / DAY {
            return None;
        }
        let (year, month, day) = civil_from_days(days);
        Self::new(i32::try_from(year).ok()?, month, day)
    }

    /// The same day of the month `months` months earlier, or that month's
    /// last day when it is shorter.
    pub fn checked_sub_months(self, months: u32) -> Option<Self> {
        let months = i64::from(i32::try_from(months).ok()?);
        let month_number = i64::from(self.year) * 12 + i64::from(self.month) - 1 - months;
        let year = month_number.div_euclid(12);
        let month = month_number.rem_euclid(12) as u8 + 1;
        Self::new(
            i32::try_from(year).ok()?,
            month,
            self.day.min(days_in_month(year, month)),
        )
    }

    /// The Unix time at which this day starts in `zone`. Days that skip
    /// midnight (a daylight-saving jump) start at the first valid local
    /// time after it.
    pub fn start_in(self, zone: &TimeZone) -> Option<i64> {
        (0..=2).find_map(|hour| self.time_in(hour, zone))
    }

    /// The Unix time at which clocks in `zone` show `hour` o'clock on this
    /// day; `None` when they skip it (a daylight-saving jump).
    pub fn time_in(self, hour: u8, zone: &TimeZone) -> Option<i64> {
        local_instant(zone, self.day_number() * DAY + i64::from(hour) * 3_600)
    }

    /// Days from 1 January 1970 to this day.
    fn day_number(self) -> i64 {
        days_from_civil(i64::from(self.year), self.month, self.day)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}-{:02}-{:02}",
            year_text(i64::from(self.year)),
            self.month,
            self.day
        )
    }
}

/// The current time in the system's time zone.
pub fn now() -> ZonedDateTime {
    ZonedDateTime::now()
}

/// The current time in a zone fixed at the system time zone's offset right
/// now, for a frontend that shows every date at that one offset.
pub fn now_at_fixed_offset() -> ZonedDateTime {
    let now = ZonedDateTime::now();
    now.with_time_zone(TimeZone::fixed(now.offset()))
}

/// The instant an RFC 3339 timestamp names, as a clock in a zone fixed at
/// the timestamp's own offset: a fixed clock for tests and fixtures.
pub fn clock_at(rfc3339: &str) -> Option<ZonedDateTime> {
    let (stamp, offset) = text::rfc3339_with_offset(rfc3339.trim())?;
    let instant = Timestamp::new(stamp.seconds, (stamp.nanos % 1_000_000_000) as i32).ok()?;
    let offset = Offset::from_seconds(i32::try_from(offset).ok()?).ok()?;
    Some(instant.to_zoned_datetime(TimeZone::fixed(offset)))
}

/// The system's time zone, which follows its daylight-saving rules.
pub fn system_zone() -> TimeZone {
    TimeZone::system()
}

/// Parses a stored timestamp.
///
/// Accepts RFC 3339 and SQLite's `YYYY-MM-DD HH:MM:SS` (read as UTC).
/// Returns `None` for anything else, so callers can fall back to showing
/// the raw value.
pub fn parse_timestamp(value: &str) -> Option<Stamp> {
    let value = value.trim();
    text::rfc3339(value).or_else(|| text::sqlite(value))
}

/// Parses the date of a mail or HTTP header (RFC 2822, with the obsolete
/// forms of its section 4.3).
pub fn parse_rfc2822(value: &str) -> Option<Stamp> {
    text::rfc2822(value)
}

/// Parses the Netscape cookie date, `Wed, 21-Oct-2015 07:28:00 GMT`.
pub fn parse_netscape_date(value: &str) -> Option<Stamp> {
    text::netscape(value)
}

/// Formats a stored timestamp as a short relative label for a message
/// list (see the module docs for the tiers).
///
/// `now` sets both the reference time and the viewer's time zone; the
/// shell passes [`now`]. Unparseable input is returned unchanged.
pub fn format_list_date(timestamp: &str, now: &ZonedDateTime, hour_cycle: HourCycle) -> String {
    let Some(parsed) = parse_timestamp(timestamp) else {
        return timestamp.to_owned();
    };
    let local = parsed.fields_in(now.time_zone());
    let today = Stamp::from(now).fields(i64::from(now.offset().seconds()));

    if local.days == today.days {
        return format_time(&local, hour_cycle);
    }
    if local.days > today.days {
        return format_day_month(&local, local.year != today.year);
    }
    match today.days - local.days {
        1 => "Yesterday".to_owned(),
        2..=6 => local.weekday().to_owned(),
        _ => format_day_month(&local, local.year != today.year),
    }
}

/// Formats a stored timestamp in full, for tooltips and quoted replies:
/// `Thu 15 Oct 2026, 10:42`.
///
/// The timestamp is shown in `zone`'s time zone. Unparseable input is
/// returned unchanged.
pub fn format_full_date(timestamp: &str, zone: &TimeZone, hour_cycle: HourCycle) -> String {
    let Some(parsed) = parse_timestamp(timestamp) else {
        return timestamp.to_owned();
    };
    let local = parsed.fields_in(zone);
    format!(
        "{} {}, {}",
        local.weekday(),
        format_day_month(&local, true),
        format_time(&local, hour_cycle)
    )
}

/// The first instant of the day after `now`, in `now`'s time zone; the
/// shell re-renders then so "Today" labels roll over to "Yesterday".
///
/// Falls back to 24 hours from `now` if local midnight does not exist (a
/// DST jump over midnight).
pub fn next_local_midnight(now: &ZonedDateTime) -> ZonedDateTime {
    let tomorrow = (Date::of(now).day_number() + 1) * DAY;
    local_instant(now.time_zone(), tomorrow)
        .and_then(|midnight| Timestamp::from_second(midnight).ok())
        .map(|midnight| midnight.to_zoned_datetime(now.time_zone().clone()))
        .unwrap_or_else(|| now.saturating_add(SignedDuration::from_hours(24)))
}

/// How long from `earlier` to `later`; `None` when `later` is the earlier.
pub fn duration_between(
    earlier: &ZonedDateTime,
    later: &ZonedDateTime,
) -> Option<std::time::Duration> {
    std::time::Duration::try_from(later.duration_since(earlier)).ok()
}

/// The instant at which clocks in `zone` show the date and time that is
/// `local` seconds after 1 January 1970 on the calendar, as seconds since
/// the Unix epoch.
///
/// Where clocks go back and show the time twice, this is the second time
/// they do; where they jump forward, the time they jump from is the
/// instant of the jump, and the times they skip are `None`. That is how
/// the chrono crate resolved local times for Zmail, and day boundaries in
/// searches depend on it.
fn local_instant(zone: &TimeZone, local: i64) -> Option<i64> {
    let instant = match zone.to_fixed_offset() {
        Ok(offset) => local.checked_sub(i64::from(offset.seconds()))?,
        Err(_) => {
            let (shifted, shift) = within_reach(local);
            let civil = TimeZone::UTC.to_datetime(Timestamp::from_second(shifted).ok()?);
            let ambiguous = zone.to_ambiguous_timestamp(civil);
            let instant = match ambiguous.offset() {
                AmbiguousOffset::Unambiguous { .. } => ambiguous.unambiguous().ok()?,
                AmbiguousOffset::Fold { .. } => ambiguous.later().ok()?,
                AmbiguousOffset::Gap { before, .. } => {
                    // Read with the offset before the jump, only the time
                    // the jump starts from is still at that offset a
                    // moment earlier.
                    let jump = ambiguous.later().ok()?;
                    let just_before = jump.checked_sub(SignedDuration::from_secs(1)).ok()?;
                    if zone.to_offset(just_before) != before {
                        return None;
                    }
                    jump
                }
            };
            instant.as_second() + shift
        }
    };
    (MIN_SECONDS..=MAX_SECONDS)
        .contains(&instant)
        .then_some(instant)
}

/// Formats seconds since the Unix epoch as a UTC RFC 3339 timestamp,
/// `2026-10-15T10:42:00Z`, the form the local store writes for its own
/// rows. Strings in this form sort chronologically.
pub fn utc_timestamp(seconds_since_epoch: u64) -> String {
    i64::try_from(seconds_since_epoch)
        .ok()
        .and_then(Stamp::from_seconds)
        .unwrap_or(Stamp::EPOCH)
        .format_utc()
}

/// The Gregorian calendar repeats every 400 years, weekdays included.
const CALENDAR_CYCLE: i64 = 146_097 * DAY;

/// `seconds` moved by whole calendar cycles to within the years the time
/// library covers (-9999 to 9999, with room to spare), and the seconds to
/// add back. Time zone rules that far out are their recurring ones, which
/// repeat with the calendar.
fn within_reach(seconds: i64) -> (i64, i64) {
    const FIRST: i64 = -370_000_000_000;
    const LAST: i64 = 250_000_000_000;
    let cycles = if seconds > LAST {
        (seconds - LAST + CALENDAR_CYCLE - 1) / CALENDAR_CYCLE
    } else if seconds < FIRST {
        -((FIRST - seconds + CALENDAR_CYCLE - 1) / CALENDAR_CYCLE)
    } else {
        0
    };
    (seconds - cycles * CALENDAR_CYCLE, cycles * CALENDAR_CYCLE)
}

/// `zone`'s offset from UTC, in seconds, at `seconds` since the epoch.
fn offset_at(zone: &TimeZone, seconds: i64) -> i64 {
    let (shifted, _) = within_reach(seconds);
    let offset = Timestamp::from_second(shifted).map_or(Offset::UTC, |at| zone.to_offset(at));
    i64::from(offset.seconds())
}

fn format_time(local: &Fields, hour_cycle: HourCycle) -> String {
    match hour_cycle {
        HourCycle::H24 => format!("{:02}:{:02}", local.hour, local.minute),
        HourCycle::H12 => format!(
            "{}:{:02} {}",
            match local.hour % 12 {
                0 => 12,
                hour => hour,
            },
            local.minute,
            if local.hour < 12 { "AM" } else { "PM" }
        ),
    }
}

fn format_day_month(local: &Fields, with_year: bool) -> String {
    if with_year {
        format!(
            "{} {} {}",
            local.day,
            local.month_name(),
            year_text(local.year)
        )
    } else {
        format!("{} {}", local.day, local.month_name())
    }
}

#[cfg(test)]
mod tests {
    use jiff::tz::Offset;

    use super::{
        HourCycle, Stamp, TimeZone, ZonedDateTime, clock_at, format_full_date, format_list_date,
        next_local_midnight, parse_timestamp,
    };

    fn utc(rfc3339: &str) -> ZonedDateTime {
        clock_at(rfc3339)
            .expect("valid test timestamp")
            .with_time_zone(TimeZone::UTC)
    }

    fn zone(hours: i32) -> TimeZone {
        TimeZone::fixed(Offset::from_seconds(hours * 3600).expect("valid offset"))
    }

    /// The local time `year`-`month`-`day` `hour`:`minute`:`second` in
    /// `zone`.
    fn local(zone: TimeZone, date: (i16, i8, i8), time: (i8, i8, i8)) -> ZonedDateTime {
        jiff::civil::date(date.0, date.1, date.2)
            .at(time.0, time.1, time.2, 0)
            .to_zoned_datetime(zone)
            .expect("valid time")
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
        let now_plus_two = local(zone(2), (2026, 10, 15), (9, 0, 0));
        assert_eq!(format_list_date(ts, &now_plus_two, HourCycle::H24), "01:30");
        // ...but still the 14th in UTC-5, a day before that viewer's today.
        let now_minus_five = local(zone(-5), (2026, 10, 15), (9, 0, 0));
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
        let now = local(zone(3), (2027, 1, 1), (12, 0, 0));
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
        assert_eq!(parsed, Stamp::from(&utc("2026-10-15T10:42:00Z")));
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
        assert_eq!(format_full_date("", &TimeZone::UTC, HourCycle::H24), "");
    }

    #[test]
    fn full_date_for_tooltips() {
        let ts = "2026-10-15T15:05:00Z";
        assert_eq!(
            format_full_date(ts, &TimeZone::UTC, HourCycle::H24),
            "Thu 15 Oct 2026, 15:05"
        );
        assert_eq!(
            format_full_date(ts, &zone(2), HourCycle::H12),
            "Thu 15 Oct 2026, 5:05 PM"
        );
    }

    #[test]
    fn next_midnight_is_the_start_of_tomorrow() {
        let now = local(zone(1), (2026, 12, 31), (23, 59, 30));
        let midnight = next_local_midnight(&now);
        assert_eq!(rfc3339(&midnight), "2027-01-01T00:00:00+01:00");
        assert_eq!(midnight.duration_since(&now).as_secs(), 30);
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
    fn clock(now: &str) -> ZonedDateTime {
        clock_at(now).expect("valid test clock")
    }

    fn rfc3339(time: &ZonedDateTime) -> String {
        time.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
    }

    /// A parsed timestamp as whole seconds since the epoch and the
    /// nanoseconds after them. A leap second (`:60`) is the last second of
    /// its minute with a further full second of nanoseconds.
    fn stamp(value: &str) -> Option<(i64, u32)> {
        parse_timestamp(value).map(|at| (at.seconds, at.nanos))
    }

    fn midnight_after(now: &str) -> String {
        rfc3339(&next_local_midnight(&clock(now)))
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
                "list date of {timestamp:?} at {now:?}"
            );
            assert_eq!(
                format_full_date(timestamp, now.time_zone(), *hour_cycle),
                *full,
                "full date of {timestamp:?} at {now:?}"
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

    // Daylight-saving rules, in named zones (the time zone database:
    // the system's, or on Windows the copy built into the time library).
    // The expected values are what chrono's `Local` gave under the same
    // `TZ`, measured before the move off it (issue #489).

    fn named(zone: &str) -> TimeZone {
        TimeZone::get(zone).expect("the time zone database has the zone")
    }

    /// The clock at `rfc3339`, for a viewer in `zone`.
    fn clock_in(zone: &str, rfc3339: &str) -> ZonedDateTime {
        clock(rfc3339).with_time_zone(named(zone))
    }

    #[test]
    fn dates_follow_daylight_saving_time() {
        use HourCycle::H24;
        // (zone, timestamp, clock, list label, full date)
        let cases = [
            // Winter and summer either side of the Irish and US changes.
            (
                "Europe/Dublin",
                "2026-01-15T12:00:00Z",
                "2026-07-01T09:00:00Z",
                "15 Jan",
                "Thu 15 Jan 2026, 12:00",
            ),
            (
                "Europe/Dublin",
                "2026-07-01T12:00:00Z",
                "2026-07-01T20:00:00Z",
                "13:00",
                "Wed 1 Jul 2026, 13:00",
            ),
            (
                "Europe/Dublin",
                "2026-03-29T00:59:59Z",
                "2026-03-29T12:00:00Z",
                "00:59",
                "Sun 29 Mar 2026, 00:59",
            ),
            (
                "Europe/Dublin",
                "2026-03-29T01:00:00Z",
                "2026-03-29T12:00:00Z",
                "02:00",
                "Sun 29 Mar 2026, 02:00",
            ),
            (
                "Europe/Dublin",
                "2026-10-25T00:30:00Z",
                "2026-10-25T12:00:00Z",
                "01:30",
                "Sun 25 Oct 2026, 01:30",
            ),
            (
                "Europe/Dublin",
                "2026-10-25T01:30:00Z",
                "2026-10-25T12:00:00Z",
                "01:30",
                "Sun 25 Oct 2026, 01:30",
            ),
            (
                "Europe/Dublin",
                "2026-06-30T23:30:00Z",
                "2026-07-01T09:00:00Z",
                "00:30",
                "Wed 1 Jul 2026, 00:30",
            ),
            (
                "America/New_York",
                "2026-03-08T06:59:00Z",
                "2026-03-08T16:00:00Z",
                "01:59",
                "Sun 8 Mar 2026, 01:59",
            ),
            (
                "America/New_York",
                "2026-03-08T07:00:00Z",
                "2026-03-08T16:00:00Z",
                "03:00",
                "Sun 8 Mar 2026, 03:00",
            ),
            (
                "America/New_York",
                "2026-11-01T05:30:00Z",
                "2026-11-01T16:00:00Z",
                "01:30",
                "Sun 1 Nov 2026, 01:30",
            ),
            (
                "America/New_York",
                "2026-11-01T06:30:00Z",
                "2026-11-01T16:00:00Z",
                "01:30",
                "Sun 1 Nov 2026, 01:30",
            ),
            (
                "America/New_York",
                "2026-11-01T03:30:00Z",
                "2026-11-01T16:00:00Z",
                "Yesterday",
                "Sat 31 Oct 2026, 23:30",
            ),
            // Half-hour daylight saving, and a zone that skipped a day.
            (
                "Australia/Lord_Howe",
                "2026-01-15T12:00:00Z",
                "2026-01-15T13:30:00Z",
                "Yesterday",
                "Thu 15 Jan 2026, 23:00",
            ),
            (
                "Australia/Lord_Howe",
                "2026-07-15T12:00:00Z",
                "2026-07-15T13:30:00Z",
                "Yesterday",
                "Wed 15 Jul 2026, 22:30",
            ),
            (
                "Pacific/Apia",
                "2011-12-30T09:59:00Z",
                "2011-12-31T12:00:00Z",
                "Thu",
                "Thu 29 Dec 2011, 23:59",
            ),
            (
                "Pacific/Apia",
                "2011-12-30T10:00:00Z",
                "2011-12-31T12:00:00Z",
                "Yesterday",
                "Sat 31 Dec 2011, 00:00",
            ),
        ];
        for (zone, timestamp, now, list, full) in cases {
            let now = clock_in(zone, now);
            assert_eq!(
                format_list_date(timestamp, &now, H24),
                list,
                "{timestamp} in {zone}"
            );
            assert_eq!(
                format_full_date(timestamp, &named(zone), H24),
                full,
                "{timestamp} in {zone}"
            );
        }
    }

    #[test]
    fn midnight_follows_daylight_saving_time() {
        // (zone, clock, the next local midnight in UTC)
        let cases = [
            (
                "Europe/Dublin",
                "2026-07-01T12:00:00Z",
                "2026-07-01T23:00:00Z",
            ),
            (
                "Europe/Dublin",
                "2026-12-01T12:00:00Z",
                "2026-12-02T00:00:00Z",
            ),
            // The day is 23 or 25 hours long.
            (
                "Europe/Dublin",
                "2026-03-29T00:30:00Z",
                "2026-03-29T23:00:00Z",
            ),
            (
                "Europe/Dublin",
                "2026-10-25T00:30:00Z",
                "2026-10-26T00:00:00Z",
            ),
            // Clocks jump from midnight to 01:00: midnight is the jump.
            (
                "America/Havana",
                "2025-03-08T20:00:00Z",
                "2025-03-09T05:00:00Z",
            ),
            (
                "America/Sao_Paulo",
                "2018-11-03T20:00:00Z",
                "2018-11-04T03:00:00Z",
            ),
            (
                "Asia/Kathmandu",
                "1985-12-31T12:00:00Z",
                "1985-12-31T18:30:00Z",
            ),
            // Clocks go back from 01:00 to midnight: the second midnight.
            (
                "America/Havana",
                "2025-11-01T20:00:00Z",
                "2025-11-02T05:00:00Z",
            ),
            // A day that never happened: the next day's start.
            (
                "Pacific/Apia",
                "2011-12-30T08:00:00Z",
                "2011-12-30T10:00:00Z",
            ),
        ];
        for (zone, now, expected) in cases {
            let midnight = next_local_midnight(&clock_in(zone, now));
            assert_eq!(
                rfc3339(&midnight.with_time_zone(TimeZone::UTC)),
                expected.replace('Z', "+00:00"),
                "after {now} in {zone}"
            );
        }
    }
}
