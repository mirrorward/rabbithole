//! `NEWNEWS`/`NEWGROUPS` date-and-time argument parsing (RFC 3977 §7.3, §7.4).
//!
//! Both commands carry a *date* (`yymmdd` or `yyyymmdd`), a *time* (`hhmmss`),
//! and an optional `GMT` flag. This module turns those textual arguments into a
//! validated, calendar-checked [`DateTimeSpec`] without pulling in a calendar
//! crate and without ever consulting the wall clock itself.
//!
//! # Two-digit years
//!
//! RFC 3977 §7.3 requires a two-digit year to be read as the year *closest to
//! the date on which the command was issued*. That decision needs a reference
//! point, so [`parse`] takes an explicit `reference_year`: the century is chosen
//! to minimise the distance from that reference (see
//! [`expand_two_digit_year`]). Keeping the reference an argument leaves this
//! module pure and deterministic — the networking layer supplies "now".
//!
//! # Robustness
//!
//! Every field is range-checked (months 1–12, days against the real length of
//! the month including leap Februaries, hours 0–23, minutes 0–59, seconds 0–60
//! to admit a leap second). Any non-digit, wrong-length, or out-of-range field
//! is reported through [`DateTimeError`]; parsing never panics.

use thiserror::Error;

/// A validated `NEWNEWS`/`NEWGROUPS` date-time argument.
///
/// The `gmt` flag records whether the client appended `GMT`; when it is
/// `false` the instant is, per RFC 3977, in the server's local time zone. This
/// type intentionally carries no time-zone offset of its own — it is a faithful
/// decomposition of the wire arguments, nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTimeSpec {
    /// Full four-digit year (two-digit input already expanded).
    pub year: i32,
    /// Month of the year, 1–12.
    pub month: u8,
    /// Day of the month, 1–31 (validated against the month's real length).
    pub day: u8,
    /// Hour of the day, 0–23.
    pub hour: u8,
    /// Minute of the hour, 0–59.
    pub minute: u8,
    /// Second of the minute, 0–60 (60 permits a leap second).
    pub second: u8,
    /// Whether the `GMT` suffix was present (else server-local time).
    pub gmt: bool,
}

/// Reasons a date/time argument pair could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DateTimeError {
    /// The date field was not 6 (`yymmdd`) or 8 (`yyyymmdd`) digits long.
    #[error("date must be 6 or 8 digits, got {0}")]
    BadDateLength(usize),
    /// The time field was not exactly 6 (`hhmmss`) digits long.
    #[error("time must be 6 digits, got {0}")]
    BadTimeLength(usize),
    /// A date or time field contained a non-digit octet.
    #[error("date/time contains a non-digit character")]
    NonDigit,
    /// The month was not in 1–12.
    #[error("month out of range: {0}")]
    MonthRange(u8),
    /// The day was not in 1..=(days in that month).
    #[error("day out of range for month: {0}")]
    DayRange(u8),
    /// The hour was not in 0–23.
    #[error("hour out of range: {0}")]
    HourRange(u8),
    /// The minute was not in 0–59.
    #[error("minute out of range: {0}")]
    MinuteRange(u8),
    /// The second was not in 0–60.
    #[error("second out of range: {0}")]
    SecondRange(u8),
}

/// Expand a two-digit year to the four-digit year closest to `reference_year`.
///
/// The candidate years are `yy` in the century before, the century of, and the
/// century after `reference_year`; the one nearest the reference wins. Exact
/// ties (a candidate 50 years on each side) resolve to the **earlier** year,
/// matching the conservative "assume the recent past" reading of RFC 3977.
///
/// # Examples
///
/// ```
/// use rabbithole_legacy_nntp::datetime::expand_two_digit_year;
///
/// assert_eq!(expand_two_digit_year(26, 2026), 2026);
/// assert_eq!(expand_two_digit_year(99, 2026), 1999); // closer than 2099
/// assert_eq!(expand_two_digit_year(1, 2099), 2101); // closer than 2001
/// ```
#[must_use]
pub fn expand_two_digit_year(yy: u8, reference_year: i32) -> i32 {
    let yy = i32::from(yy);
    let century = reference_year - reference_year.rem_euclid(100);
    let candidates = [century - 100 + yy, century + yy, century + 100 + yy];
    let mut best = candidates[0];
    let mut best_dist = (best - reference_year).abs();
    for &cand in &candidates[1..] {
        let dist = (cand - reference_year).abs();
        // Strictly-closer only, so a tie keeps the earlier (smaller) candidate.
        if dist < best_dist {
            best = cand;
            best_dist = dist;
        }
    }
    best
}

/// Number of days in `month` (1–12) of `year`, accounting for leap years.
///
/// Returns 0 for a month outside 1–12 so callers can treat it as "no valid
/// day". Uses the proleptic Gregorian leap rule.
#[must_use]
pub fn days_in_month(year: i32, month: u8) -> u8 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    }
}

/// Read `len` ASCII digits from `bytes` at `off` into a `u32`.
///
/// The caller must have already checked that the slice is all digits and long
/// enough; this is only reached on validated input.
fn read_num(bytes: &[u8], off: usize, len: usize) -> u32 {
    let mut acc = 0u32;
    for &b in &bytes[off..off + len] {
        acc = acc * 10 + u32::from(b - b'0');
    }
    acc
}

/// Parse a `NEWNEWS`/`NEWGROUPS` date/time argument pair.
///
/// `date` is `yymmdd` or `yyyymmdd`, `time` is `hhmmss`, `gmt` records the
/// optional `GMT` flag, and `reference_year` selects the century for two-digit
/// years (see [`expand_two_digit_year`]).
///
/// # Errors
///
/// Returns [`DateTimeError`] on wrong length, a non-digit octet, or any
/// out-of-range calendar/clock field. Never panics on arbitrary input.
pub fn parse(
    date: &str,
    time: &str,
    gmt: bool,
    reference_year: i32,
) -> Result<DateTimeSpec, DateTimeError> {
    let db = date.as_bytes();
    if !db.iter().all(u8::is_ascii_digit) {
        return Err(DateTimeError::NonDigit);
    }
    let (year, month_off) = match db.len() {
        6 => (
            expand_two_digit_year(read_num(db, 0, 2) as u8, reference_year),
            2,
        ),
        8 => (read_num(db, 0, 4) as i32, 4),
        other => return Err(DateTimeError::BadDateLength(other)),
    };
    let month = read_num(db, month_off, 2) as u8;
    let day = read_num(db, month_off + 2, 2) as u8;

    let tb = time.as_bytes();
    if tb.len() != 6 {
        return Err(DateTimeError::BadTimeLength(tb.len()));
    }
    if !tb.iter().all(u8::is_ascii_digit) {
        return Err(DateTimeError::NonDigit);
    }
    let hour = read_num(tb, 0, 2) as u8;
    let minute = read_num(tb, 2, 2) as u8;
    let second = read_num(tb, 4, 2) as u8;

    if !(1..=12).contains(&month) {
        return Err(DateTimeError::MonthRange(month));
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(DateTimeError::DayRange(day));
    }
    if hour > 23 {
        return Err(DateTimeError::HourRange(hour));
    }
    if minute > 59 {
        return Err(DateTimeError::MinuteRange(minute));
    }
    if second > 60 {
        return Err(DateTimeError::SecondRange(second));
    }

    Ok(DateTimeSpec {
        year,
        month,
        day,
        hour,
        minute,
        second,
        gmt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_four_digit_year() {
        assert_eq!(
            parse("20260701", "123456", true, 2026),
            Ok(DateTimeSpec {
                year: 2026,
                month: 7,
                day: 1,
                hour: 12,
                minute: 34,
                second: 56,
                gmt: true,
            })
        );
    }

    #[test]
    fn parses_two_digit_year_with_reference() {
        let spec = parse("260701", "000000", false, 2026).unwrap();
        assert_eq!(spec.year, 2026);
        assert!(!spec.gmt);
        // Same digits, reference in a different century.
        assert_eq!(parse("990101", "000000", false, 2026).unwrap().year, 1999);
        assert_eq!(parse("010101", "000000", false, 2099).unwrap().year, 2101);
    }

    #[test]
    fn expand_year_tie_prefers_earlier() {
        // 76 is exactly 50 years from 2026 in both directions (1976 vs 2076).
        assert_eq!(expand_two_digit_year(76, 2026), 1976);
    }

    #[test]
    fn accepts_leap_day_and_leap_second() {
        assert!(parse("20240229", "235960", true, 2024).is_ok());
    }

    #[test]
    fn rejects_non_leap_feb_29() {
        assert_eq!(
            parse("20250229", "000000", true, 2025),
            Err(DateTimeError::DayRange(29))
        );
        // 1900 is not a leap year (divisible by 100, not 400).
        assert_eq!(
            parse("19000229", "000000", true, 1900),
            Err(DateTimeError::DayRange(29))
        );
        // 2000 is a leap year (divisible by 400).
        assert!(parse("20000229", "000000", true, 2000).is_ok());
    }

    #[test]
    fn rejects_out_of_range_fields() {
        assert_eq!(
            parse("20261301", "000000", true, 2026),
            Err(DateTimeError::MonthRange(13))
        );
        assert_eq!(
            parse("20260700", "000000", true, 2026),
            Err(DateTimeError::DayRange(0))
        );
        assert_eq!(
            parse("20260701", "240000", true, 2026),
            Err(DateTimeError::HourRange(24))
        );
        assert_eq!(
            parse("20260701", "006000", true, 2026),
            Err(DateTimeError::MinuteRange(60))
        );
        assert_eq!(
            parse("20260701", "000061", true, 2026),
            Err(DateTimeError::SecondRange(61))
        );
    }

    #[test]
    fn rejects_bad_lengths_and_non_digits() {
        assert_eq!(
            parse("2026070", "000000", true, 2026),
            Err(DateTimeError::BadDateLength(7))
        );
        assert_eq!(
            parse("20260701", "00000", true, 2026),
            Err(DateTimeError::BadTimeLength(5))
        );
        assert_eq!(
            parse("2026070x", "000000", true, 2026),
            Err(DateTimeError::NonDigit)
        );
        assert_eq!(
            parse("20260701", "0000x0", true, 2026),
            Err(DateTimeError::NonDigit)
        );
    }

    #[test]
    fn days_in_month_edges() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(days_in_month(2026, 0), 0);
        assert_eq!(days_in_month(2026, 13), 0);
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for date in ["", "0", "abcdef", "\0\0\0\0\0\0", "999999", "00000000"] {
            for time in ["", "000000", "zzzzzz", "\0\0\0\0\0\0"] {
                let _ = parse(date, time, false, 2026);
                let _ = parse(date, time, true, 0);
            }
        }
    }
}
