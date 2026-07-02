//! Lenient feed-date parsing to unix seconds — no chrono.
//!
//! RSS 2.0 uses RFC 2822/822 dates (`Tue, 10 Jun 2003 04:00:00 GMT`),
//! Atom 1.0 uses RFC 3339 (`2003-12-13T18:30:02Z`), and real feeds bend
//! both: 2-digit years, missing seconds, named US timezones, lowercase
//! `t`/`z`, a space instead of `T`, missing zones. The parsers here accept
//! all of that and answer `Option<i64>` — a date that cannot be understood
//! is simply absent, never an error and never a panic.
//!
//! Civil-date conversion uses Howard Hinnant's `days_from_civil`
//! algorithm; years are clamped to 1..=9999 so arithmetic cannot
//! overflow.

/// Try RFC 2822 first, then RFC 3339. The two grammars are disjoint
/// (2822 starts with a name or day number, 3339 with `YYYY-`), so order
/// only affects speed.
pub fn parse_date_lenient(s: &str) -> Option<i64> {
    parse_rfc2822(s).or_else(|| parse_rfc3339(s))
}

/// Lenient RFC 2822 / RFC 822: optional weekday, 2/3/4-digit years,
/// optional seconds, numeric or named zones (missing/unknown zone reads
/// as UTC, matching RFC 5322's `-0000` semantics).
pub fn parse_rfc2822(input: &str) -> Option<i64> {
    let s = input.trim();
    // Drop an optional leading day-of-week ("Tue," / "Tuesday,").
    let s = match s.find(',') {
        Some(i) => &s[i + 1..],
        None => s,
    };
    let mut tok = s.split_whitespace();
    let day: i64 = tok.next()?.parse().ok()?;
    let month = month_number(tok.next()?)?;
    let year_tok = tok.next()?;
    let mut year: i64 = year_tok.parse().ok()?;
    if year_tok.len() <= 2 {
        year += if year < 50 { 2000 } else { 1900 };
    } else if year_tok.len() == 3 {
        year += 1900; // RFC 5322 obsolete 3-digit form
    }
    let (h, m, sec) = parse_hms(tok.next()?)?;
    // Unknown named zones read as UTC per RFC 5322 §4.3.
    let offset = match tok.next() {
        Some(z) => zone_offset_secs(z).unwrap_or(0),
        None => 0,
    };
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + h * 3_600 + m * 60 + sec - offset)
}

/// Lenient RFC 3339: `Z`/`z`, `±hh:mm` or `±hhmm` offsets, fractional
/// seconds (truncated), lowercase `t` or a space as the separator, and —
/// beyond the RFC — a missing zone (read as UTC) or a bare date
/// (read as midnight UTC).
pub fn parse_rfc3339(input: &str) -> Option<i64> {
    let s = input.trim();
    let date = s.get(..10)?;
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: i64 = dp.next()?.parse().ok()?;
    let day: i64 = dp.next()?.parse().ok()?;
    if dp.next().is_some() {
        return None;
    }
    let days = days_from_civil(year, month, day)?;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some(days * 86_400);
    }
    let sep = rest.chars().next()?;
    if !matches!(sep, 'T' | 't' | ' ') {
        return None;
    }
    let (time_part, offset) = split_zone(&rest[1..])?;
    // Truncate fractional seconds.
    let (h, m, sec) = parse_hms(time_part.split('.').next()?)?;
    Some(days * 86_400 + h * 3_600 + m * 60 + sec - offset)
}

/// Split "hh:mm:ss[.frac][zone]" into the time text and the zone offset
/// in seconds. A missing zone reads as UTC.
fn split_zone(rest: &str) -> Option<(&str, i64)> {
    if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        return Some((t, 0));
    }
    // '-' cannot occur in the time-of-day part, and '+' only in a zone,
    // so the rightmost sign (if any) starts the offset.
    if let Some(i) = rest.rfind(['+', '-']) {
        let (time, zone) = rest.split_at(i);
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let digits = &zone[1..];
        let (hh, mm) = match digits.split_once(':') {
            Some(pair) => pair,
            None if digits.len() == 4 => (&digits[..2], &digits[2..]),
            None => return None,
        };
        let h: i64 = hh.parse().ok()?;
        let m: i64 = mm.parse().ok()?;
        if h > 14 || m > 59 {
            return None;
        }
        return Some((time, sign * (h * 3_600 + m * 60)));
    }
    Some((rest, 0))
}

/// "hh:mm[:ss]" with a leap-second clamped to :59.
fn parse_hms(t: &str) -> Option<(i64, i64, i64)> {
    let mut it = t.split(':');
    let h: i64 = it.next()?.trim().parse().ok()?;
    let m: i64 = it.next()?.trim().parse().ok()?;
    let s: i64 = match it.next() {
        Some(x) => x.trim().parse().ok()?,
        None => 0,
    };
    if it.next().is_some() {
        return None;
    }
    ((0..24).contains(&h) && (0..60).contains(&m) && (0..=60).contains(&s)).then_some((
        h,
        m,
        s.min(59),
    ))
}

fn month_number(name: &str) -> Option<i64> {
    let key = name.get(..3)?;
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    months
        .iter()
        .position(|m| m.eq_ignore_ascii_case(key))
        .map(|i| i as i64 + 1)
}

fn zone_offset_secs(z: &str) -> Option<i64> {
    let z = z.trim();
    if let Some(digits) = z.strip_prefix(['+', '-']) {
        if digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()) {
            let h: i64 = digits[..2].parse().ok()?;
            let m: i64 = digits[2..].parse().ok()?;
            if h <= 14 && m <= 59 {
                let sign = if z.starts_with('-') { -1 } else { 1 };
                return Some(sign * (h * 3_600 + m * 60));
            }
        }
        return None;
    }
    match z.to_ascii_uppercase().as_str() {
        "UT" | "GMT" | "UTC" | "Z" => Some(0),
        "EST" => Some(-5 * 3_600),
        "EDT" => Some(-4 * 3_600),
        "CST" => Some(-6 * 3_600),
        "CDT" => Some(-5 * 3_600),
        "MST" => Some(-7 * 3_600),
        "MDT" => Some(-6 * 3_600),
        "PST" => Some(-8 * 3_600),
        "PDT" => Some(-7 * 3_600),
        _ => None,
    }
}

/// Days since 1970-01-01 for a validated civil date (Hinnant's
/// `days_from_civil`). `None` for out-of-range fields.
fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=9999).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2822_canonical() {
        // date -u -d "Tue, 10 Jun 2003 04:00:00 GMT" +%s
        assert_eq!(
            parse_rfc2822("Tue, 10 Jun 2003 04:00:00 GMT"),
            Some(1_055_217_600)
        );
    }

    #[test]
    fn rfc2822_numeric_offsets() {
        let base = parse_rfc2822("Wed, 02 Jul 2026 12:00:00 +0000").unwrap();
        assert_eq!(parse_rfc2822("Wed, 02 Jul 2026 05:00:00 -0700"), Some(base));
        assert_eq!(parse_rfc2822("Wed, 02 Jul 2026 17:30:00 +0530"), Some(base));
    }

    #[test]
    fn rfc2822_named_zones() {
        let gmt = parse_rfc2822("Fri, 21 Nov 1997 09:55:06 GMT").unwrap();
        assert_eq!(
            parse_rfc2822("Fri, 21 Nov 1997 04:55:06 EST"),
            Some(gmt),
            "EST is UTC-5"
        );
        assert_eq!(
            parse_rfc2822("Fri, 21 Nov 1997 01:55:06 PST"),
            Some(gmt),
            "PST is UTC-8"
        );
        // Unknown zone falls back to UTC rather than failing.
        assert_eq!(parse_rfc2822("Fri, 21 Nov 1997 09:55:06 XYZ"), Some(gmt));
        // Missing zone entirely also reads as UTC.
        assert_eq!(parse_rfc2822("Fri, 21 Nov 1997 09:55:06"), Some(gmt));
    }

    #[test]
    fn rfc2822_loose_forms() {
        // No weekday, no seconds, 2-digit year.
        assert_eq!(
            parse_rfc2822("10 Jun 03 04:00 GMT"),
            Some(1_055_217_600),
            "2-digit year < 50 is 20xx"
        );
        // 2-digit year >= 50 is 19xx: 10 Jun 99.
        let y1999 = parse_rfc2822("10 Jun 1999 04:00:00 GMT").unwrap();
        assert_eq!(parse_rfc2822("10 Jun 99 04:00:00 GMT"), Some(y1999));
        // Full weekday name before the comma.
        assert_eq!(
            parse_rfc2822("Tuesday, 10 Jun 2003 04:00:00 UT"),
            Some(1_055_217_600)
        );
    }

    #[test]
    fn rfc2822_rejects_nonsense() {
        for bad in [
            "",
            "not a date",
            "32 Jan 2020 00:00:00 GMT",
            "29 Feb 2023 00:00:00 GMT", // not a leap year
            "10 Zzz 2020 00:00:00 GMT",
            "10 Jun 2020 25:00:00 GMT",
            "10 Jun 2020 10:61:00 GMT",
            "10 Jun 99999999999999999999 00:00:00 GMT",
        ] {
            assert_eq!(parse_rfc2822(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn rfc3339_canonical() {
        // date -u -d "2003-12-13T18:30:02Z" +%s
        assert_eq!(parse_rfc3339("2003-12-13T18:30:02Z"), Some(1_071_340_202));
    }

    #[test]
    fn rfc3339_offsets_and_fractions() {
        let z = parse_rfc3339("2003-12-13T18:30:02Z").unwrap();
        assert_eq!(parse_rfc3339("2003-12-13T18:30:02.25Z"), Some(z));
        assert_eq!(parse_rfc3339("2003-12-13T13:30:02-05:00"), Some(z));
        assert_eq!(parse_rfc3339("2003-12-14T04:00:02+09:30"), Some(z));
        assert_eq!(
            parse_rfc3339("2003-12-13T13:30:02-0500"),
            Some(z),
            "hhmm zone"
        );
    }

    #[test]
    fn rfc3339_loose_forms() {
        let z = parse_rfc3339("2003-12-13T18:30:02Z").unwrap();
        assert_eq!(parse_rfc3339("2003-12-13t18:30:02z"), Some(z), "lowercase");
        assert_eq!(
            parse_rfc3339("2003-12-13 18:30:02"),
            Some(z),
            "space + no zone"
        );
        assert_eq!(
            parse_rfc3339("2003-12-13"),
            Some(1_071_340_202 - 18 * 3_600 - 30 * 60 - 2),
            "bare date is midnight UTC"
        );
        // Leap second clamps instead of failing.
        assert_eq!(
            parse_rfc3339("1998-12-31T23:59:60Z"),
            parse_rfc3339("1998-12-31T23:59:59Z")
        );
        // Leap day in a leap year is fine.
        assert!(parse_rfc3339("2024-02-29T00:00:00Z").is_some());
    }

    #[test]
    fn rfc3339_rejects_nonsense() {
        for bad in [
            "",
            "2003-13-01T00:00:00Z",
            "2003-02-30T00:00:00Z",
            "2003-12-13X18:30:02Z",
            "20031213",
            "junk-junk-junk",
            "2003-12-13T18:30:02+99:00",
        ] {
            assert_eq!(parse_rfc3339(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn lenient_dispatch_tries_both() {
        assert_eq!(
            parse_date_lenient("Tue, 10 Jun 2003 04:00:00 GMT"),
            Some(1_055_217_600)
        );
        assert_eq!(
            parse_date_lenient("2003-12-13T18:30:02Z"),
            Some(1_071_340_202)
        );
        assert_eq!(parse_date_lenient("yesterday-ish"), None);
    }

    #[test]
    fn epoch_and_pre_epoch() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), Some(-1));
        assert_eq!(parse_rfc2822("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
    }
}
