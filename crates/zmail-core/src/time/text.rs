//! Parsers for the timestamps Zmail stores and receives.
//!
//! Each accepts exactly the text the chrono crate (0.4.45) accepted where
//! Zmail used it, and nothing more: what a timestamp in an existing store,
//! a `Date` header or a cookie means must not depend on the time library
//! of the day. That includes chrono's leniencies (white space before a
//! number, one-digit fields in SQLite's form, a `Date` whose named zone is
//! unknown being no date). The grammars follow chrono's `format::parse` and
//! `format::scan` modules, Copyright (c) 2014-2025 Kang Seonghoon and
//! contributors, licensed under MIT or Apache-2.0 (see `about.toml`).

use super::{
    Stamp,
    calendar::{DAY, MAX_SECONDS, MIN_SECONDS, days_from_civil, is_date, weekday_from_days},
};

/// A leap second: the last second of its minute, plus this many
/// nanoseconds.
const LEAP: u32 = 1_000_000_000;

/// The fields of a date and time, as read and not yet checked.
#[derive(Default)]
struct Parts {
    /// Monday is 0.
    weekday: Option<usize>,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanos: u32,
    /// Seconds east of UTC.
    offset: i64,
}

impl Parts {
    /// The instant the fields name, when they name one.
    fn stamp(&self) -> Option<Stamp> {
        if !is_date(self.year, self.month, self.day)
            || !(0..24).contains(&self.hour)
            || !(0..60).contains(&self.minute)
            || !(0..=60).contains(&self.second)
            || self.offset.abs() >= DAY
        {
            return None;
        }
        let days = days_from_civil(self.year, self.month as u8, self.day as u8);
        if self
            .weekday
            .is_some_and(|weekday| weekday != weekday_from_days(days))
        {
            return None;
        }
        let (second, leap) = if self.second == 60 {
            (59, LEAP)
        } else {
            (self.second, 0)
        };
        let seconds = days * DAY + self.hour * 3_600 + self.minute * 60 + second - self.offset;
        (MIN_SECONDS..=MAX_SECONDS)
            .contains(&seconds)
            .then_some(Stamp {
                seconds,
                nanos: self.nanos + leap,
            })
    }
}

/// RFC 3339, as the store and the adapters write it: `T`, `t` or a space
/// between date and time, any number of fractional digits, `Z`, `z` or a
/// `+HH:MM` offset short of a day, and `:60` for a leap second.
pub(super) fn rfc3339(value: &str) -> Option<Stamp> {
    rfc3339_with_offset(value).map(|(stamp, _)| stamp)
}

/// [`rfc3339`], with the timestamp's offset in seconds east of UTC.
pub(super) fn rfc3339_with_offset(value: &str) -> Option<(Stamp, i64)> {
    let bytes = value.as_bytes();
    let fixed = bytes.get(..19)?;
    let digit = |index: usize| {
        fixed[index]
            .is_ascii_digit()
            .then(|| i64::from(fixed[index] - b'0'))
    };
    let two = |index: usize| Some(digit(index)? * 10 + digit(index + 1)?);
    let mut parts = Parts {
        year: two(0)? * 100 + two(2)?,
        month: two(5)?,
        day: two(8)?,
        hour: two(11)?,
        minute: two(14)?,
        second: two(17)?,
        ..Parts::default()
    };
    if fixed[4] != b'-'
        || fixed[7] != b'-'
        || !matches!(fixed[10], b't' | b'T' | b' ')
        || fixed[13] != b':'
        || fixed[16] != b':'
    {
        return None;
    }
    // The first 19 bytes are ASCII, so these are character boundaries.
    let rest = if bytes.get(19) == Some(&b'.') {
        let (rest, nanos) = fraction(&value[20..])?;
        parts.nanos = nanos;
        rest
    } else {
        &value[19..]
    };
    let (rest, offset) = match rest.as_bytes().first() {
        Some(b'Z' | b'z') => (&rest[1..], 0),
        _ => numeric_offset(rest, true, true)?,
    };
    parts.offset = offset;
    if !rest.is_empty() {
        return None;
    }
    Some((parts.stamp()?, offset))
}

/// SQLite's `YYYY-MM-DD HH:MM:SS`, in UTC.
pub(super) fn sqlite(value: &str) -> Option<Stamp> {
    let mut parts = Parts::default();
    let rest = year_month_day(value, &mut parts)?;
    let rest = hour_minute_second(rest.trim_start(), &mut parts)?;
    if !rest.is_empty() {
        return None;
    }
    parts.stamp()
}

/// A calendar day written `YYYY-MM-DD`, as year, month and day.
pub(super) fn date(value: &str) -> Option<(i64, u8, u8)> {
    let mut parts = Parts::default();
    let rest = year_month_day(value, &mut parts)?;
    if !rest.is_empty() || !is_date(parts.year, parts.month, parts.day) {
        return None;
    }
    Some((parts.year, parts.month as u8, parts.day as u8))
}

/// The date and time of RFC 2822, section 3.3, with the obsolete forms of
/// section 4.3: an optional day of the week, two- and three-digit years,
/// optional seconds, the North American zone names and trailing comments.
pub(super) fn rfc2822(value: &str) -> Option<Stamp> {
    let mut parts = Parts::default();
    let mut rest = value.trim_start();
    if let Some((after, weekday)) = short_weekday(rest) {
        parts.weekday = Some(weekday);
        rest = after.strip_prefix(',')?;
    }
    (rest, parts.day) = number(rest.trim_start(), 1, 2)?;
    (rest, parts.month) = short_month(spaces(rest)?)?;
    let before_year = spaces(rest)?;
    (rest, parts.year) = number(before_year, 2, usize::MAX)?;
    // `26` is 2026, `99` is 1999 and `126` is 2026; four digits or more
    // are the year itself.
    parts.year += match (before_year.len() - rest.len(), parts.year) {
        (2, 0..=49) => 2000,
        (2, _) | (3, _) => 1900,
        _ => 0,
    };
    (rest, parts.hour) = number(spaces(rest)?, 2, 2)?;
    rest = rest.trim_start().strip_prefix(':')?.trim_start();
    (rest, parts.minute) = number(rest, 2, 2)?;
    if let Some(after) = rest.trim_start().strip_prefix(':') {
        (rest, parts.second) = number(after, 2, 2)?;
    }
    (rest, parts.offset) = zone(spaces(rest)?)?;
    while let Some(after) = comment(rest) {
        rest = after;
    }
    if !rest.is_empty() {
        return None;
    }
    parts.stamp()
}

/// The Netscape cookie date, `Wed, 21-Oct-2015 07:28:00 GMT`.
pub(super) fn netscape(value: &str) -> Option<Stamp> {
    let mut parts = Parts::default();
    let (rest, weekday) = short_weekday(value)?;
    parts.weekday = Some(weekday);
    let rest = rest.strip_prefix(',')?.trim_start();
    let (rest, day) = number(rest.trim_start(), 1, 2)?;
    parts.day = day;
    let (rest, month) = short_month(rest.strip_prefix('-')?)?;
    parts.month = month;
    let (rest, year) = year(rest.strip_prefix('-')?)?;
    parts.year = year;
    let rest = hour_minute_second(rest.trim_start(), &mut parts)?;
    if rest.trim_start() != "GMT" {
        return None;
    }
    parts.stamp()
}

/// `YYYY-MM-DD` at the start of `value`, each number after any white space
/// and as short as one digit.
fn year_month_day<'a>(value: &'a str, parts: &mut Parts) -> Option<&'a str> {
    let (rest, year) = year(value)?;
    let (rest, month) = number(rest.strip_prefix('-')?.trim_start(), 1, 2)?;
    let (rest, day) = number(rest.strip_prefix('-')?.trim_start(), 1, 2)?;
    (parts.year, parts.month, parts.day) = (year, month, day);
    Some(rest)
}

/// `HH:MM:SS` at the start of `value`, each number after any white space
/// and as short as one digit.
fn hour_minute_second<'a>(value: &'a str, parts: &mut Parts) -> Option<&'a str> {
    let (rest, hour) = number(value.trim_start(), 1, 2)?;
    let (rest, minute) = number(rest.strip_prefix(':')?.trim_start(), 1, 2)?;
    let (rest, second) = number(rest.strip_prefix(':')?.trim_start(), 1, 2)?;
    (parts.hour, parts.minute, parts.second) = (hour, minute, second);
    Some(rest)
}

/// A year after any white space: up to four digits, or a sign and any
/// number of digits.
fn year(value: &str) -> Option<(&str, i64)> {
    let value = value.trim_start();
    if let Some(digits) = value.strip_prefix('-') {
        let (rest, year) = number(digits, 1, usize::MAX)?;
        Some((rest, -year))
    } else if let Some(digits) = value.strip_prefix('+') {
        number(digits, 1, usize::MAX)
    } else {
        number(value, 1, 4)
    }
}

/// Between `min` and `max` ASCII digits from the start of `value`, as a
/// number, with what follows them. `None` when there are fewer than `min`
/// or the number does not fit.
fn number(value: &str, min: usize, max: usize) -> Option<(&str, i64)> {
    let digits = value
        .bytes()
        .take(max)
        .take_while(u8::is_ascii_digit)
        .count();
    if digits < min {
        return None;
    }
    let mut number = 0i64;
    for digit in value[..digits].bytes() {
        number = number
            .checked_mul(10)?
            .checked_add(i64::from(digit - b'0'))?;
    }
    Some((&value[digits..], number))
}

/// The digits after a decimal point as nanoseconds; digits past the ninth
/// are dropped.
fn fraction(value: &str) -> Option<(&str, u32)> {
    let (rest, digits) = number(value, 1, 9)?;
    let scale = 10u32.pow((9 - (value.len() - rest.len())) as u32);
    let rest = rest.trim_start_matches(|ch: char| ch.is_ascii_digit());
    Some((rest, digits as u32 * scale))
}

/// At least one white space character, then the rest.
fn spaces(value: &str) -> Option<&str> {
    let rest = value.trim_start();
    (rest.len() < value.len()).then_some(rest)
}

/// The three letters at the start of `value`, lowercased, and the rest.
fn three_letters(value: &str) -> Option<([u8; 3], &str)> {
    let letters: [u8; 3] = value.as_bytes().get(..3)?.try_into().ok()?;
    letters.iter().all(u8::is_ascii_alphabetic).then(|| {
        (
            letters.map(|letter| letter.to_ascii_lowercase()),
            &value[3..],
        )
    })
}

/// A three-letter weekday name in any case, as its number from Monday (0).
fn short_weekday(value: &str) -> Option<(&str, usize)> {
    let (letters, rest) = three_letters(value)?;
    let weekday = [b"mon", b"tue", b"wed", b"thu", b"fri", b"sat", b"sun"]
        .iter()
        .position(|name| **name == letters)?;
    Some((rest, weekday))
}

/// A three-letter month name in any case, as its number from 1.
fn short_month(value: &str) -> Option<(&str, i64)> {
    let (letters, rest) = three_letters(value)?;
    let month = [
        b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov",
        b"dec",
    ]
    .iter()
    .position(|name| **name == letters)?;
    Some((rest, month as i64 + 1))
}

/// A `+HH:MM` (`colon`) or `+HHMM` offset, in seconds east of UTC.
/// `minus_sign` also takes U+2212 for the sign, as ISO 8601 allows.
fn numeric_offset(value: &str, colon: bool, minus_sign: bool) -> Option<(&str, i64)> {
    let (sign, rest) = match value.chars().next()? {
        '+' => (1, &value[1..]),
        '-' => (-1, &value[1..]),
        '\u{2212}' if minus_sign => (-1, &value['\u{2212}'.len_utf8()..]),
        _ => return None,
    };
    let two = |value: &str| match value.as_bytes() {
        [tens @ b'0'..=b'9', units @ b'0'..=b'9', ..] => {
            Some(i64::from(tens - b'0') * 10 + i64::from(units - b'0'))
        }
        _ => None,
    };
    let hours = two(rest)?;
    let rest = &rest[2..];
    let rest = if colon { rest.strip_prefix(':')? } else { rest };
    let minutes = two(rest).filter(|minutes| *minutes < 60)?;
    Some((&rest[2..], sign * (hours * 3_600 + minutes * 60)))
}

/// An RFC 2822 zone: a numeric offset, or one of the names section 4.3
/// keeps. Any other single letter but `J` is UTC, as the RFC recommends;
/// any other name is not a zone.
fn zone(value: &str) -> Option<(&str, i64)> {
    let letters = value.bytes().take_while(u8::is_ascii_alphabetic).count();
    if letters == 0 {
        return numeric_offset(value, false, false);
    }
    let hours = match value[..letters].to_ascii_lowercase().as_str() {
        "gmt" | "ut" | "z" => 0,
        "edt" => -4,
        "est" | "cdt" => -5,
        "cst" | "mdt" => -6,
        "mst" | "pdt" => -7,
        "pst" => -8,
        name if name.len() == 1 && name != "j" => 0,
        _ => return None,
    };
    Some((&value[letters..], hours * 3_600))
}

/// A parenthesised comment after any white space, with nested comments
/// and backslash escapes; returns what follows it.
fn comment(value: &str) -> Option<&str> {
    let value = value.trim_start();
    if !value.starts_with('(') {
        return None;
    }
    let mut depth = 0usize;
    let mut escaped = false;
    for (index, byte) in value.bytes().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&value[index + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}
