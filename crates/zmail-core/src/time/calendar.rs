//! The proleptic Gregorian calendar as plain arithmetic on day numbers.
//!
//! Stored and wire timestamps are read and written with this rather than
//! with the time library, so their text cannot change with it, and so that
//! every instant Zmail has ever accepted (years -262143 to 262142, the
//! range of the chrono crate it used before) still has one meaning.

/// The first year a date can have.
pub(super) const MIN_YEAR: i64 = -262_143;
/// The last year a date can have.
pub(super) const MAX_YEAR: i64 = 262_142;
/// Seconds in a day.
pub(super) const DAY: i64 = 86_400;
/// The first instant: midnight UTC on 1 January of [`MIN_YEAR`].
pub(super) const MIN_SECONDS: i64 = -8_334_601_228_800;
/// The last whole second: 23:59:59 UTC on 31 December of [`MAX_YEAR`].
pub(super) const MAX_SECONDS: i64 = 8_210_266_876_799;

/// English weekday abbreviations, Monday first.
pub(super) const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
/// English month abbreviations.
pub(super) const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The number of days in `month` (1 to 12) of `year`.
pub(super) fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Whether `year`, `month` and `day` name a day of the calendar.
pub(super) fn is_date(year: i64, month: i64, day: i64) -> bool {
    (MIN_YEAR..=MAX_YEAR).contains(&year)
        && (1..=12).contains(&month)
        && (1..=i64::from(days_in_month(year, month as u8))).contains(&day)
}

/// Days from 1 January 1970 to the given day, which must be a date.
pub(super) fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    // Years run from March, so a leap day is the last day of its year.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let month_from_march = (i64::from(month) + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The year, month and day that is `days` days after 1 January 1970.
pub(super) fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = (month_from_march + 2) % 12 + 1;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as u8, day as u8)
}

/// The weekday of a day number, counted from Monday as 0.
pub(super) fn weekday_from_days(days: i64) -> usize {
    // 1 January 1970 was a Thursday.
    (days + 3).rem_euclid(7) as usize
}

/// A year as chrono's `%Y` wrote it: four digits, and a sign with at
/// least four digits for years outside 0 to 9999.
pub(super) fn year_text(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{year:+05}")
    }
}

/// The fields of a time of day and its date, at some offset from UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Fields {
    /// Days from 1 January 1970 to this date.
    pub(super) days: i64,
    pub(super) year: i64,
    pub(super) month: u8,
    pub(super) day: u8,
    pub(super) hour: u8,
    pub(super) minute: u8,
    /// 60 in a leap second.
    pub(super) second: u8,
}

impl Fields {
    /// The fields of the second `seconds` after the epoch, `leap_second`
    /// when it is the leap second that follows it.
    pub(super) fn of(seconds: i64, leap_second: bool) -> Self {
        let days = seconds.div_euclid(DAY);
        let second_of_day = seconds.rem_euclid(DAY);
        let (year, month, day) = civil_from_days(days);
        Self {
            days,
            year,
            month,
            day,
            hour: (second_of_day / 3_600) as u8,
            minute: (second_of_day / 60 % 60) as u8,
            second: (second_of_day % 60) as u8 + u8::from(leap_second),
        }
    }

    pub(super) fn weekday(&self) -> &'static str {
        WEEKDAYS[weekday_from_days(self.days)]
    }

    pub(super) fn month_name(&self) -> &'static str {
        MONTHS[usize::from(self.month) - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_numbers_and_dates_agree() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2026, 10, 15), 20_741);
        assert_eq!(civil_from_days(20_741), (2026, 10, 15));
        assert_eq!(days_from_civil(2000, 2, 29), 11_016);
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(-719_528), (0, 1, 1));
        assert_eq!(days_from_civil(MIN_YEAR, 1, 1) * DAY, MIN_SECONDS);
        assert_eq!(
            days_from_civil(MAX_YEAR, 12, 31) * DAY + DAY - 1,
            MAX_SECONDS
        );
        // Every day of eight centuries, across the 1900 and 2000 rules.
        let mut expected = (1600, 1, 1);
        for days in days_from_civil(1600, 1, 1)..days_from_civil(2400, 1, 1) {
            assert_eq!(civil_from_days(days), expected);
            let (year, month, day) = expected;
            assert_eq!(days_from_civil(year, month, day), days);
            expected = if day < days_in_month(year, month) {
                (year, month, day + 1)
            } else if month < 12 {
                (year, month + 1, 1)
            } else {
                (year + 1, 1, 1)
            };
        }
    }

    #[test]
    fn weekdays_and_years_read_as_chrono_wrote_them() {
        assert_eq!(WEEKDAYS[weekday_from_days(0)], "Thu");
        assert_eq!(WEEKDAYS[weekday_from_days(20_741)], "Thu");
        assert_eq!(WEEKDAYS[weekday_from_days(-1)], "Wed");
        assert_eq!(WEEKDAYS[weekday_from_days(-719_528)], "Sat");
        assert_eq!(year_text(2026), "2026");
        assert_eq!(year_text(999), "0999");
        assert_eq!(year_text(0), "0000");
        assert_eq!(year_text(-1), "-0001");
        assert_eq!(year_text(10_000), "+10000");
        assert_eq!(year_text(-262_143), "-262143");
    }
}
