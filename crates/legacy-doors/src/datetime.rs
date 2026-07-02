//! Minimal UTC civil date/time formatting from [`std::time::SystemTime`].
//!
//! DOOR.SYS carries a "last call date" (`MM/DD/YY`) and "time of this call"
//! (`HH:MM`). Rather than pull in a date crate (this crate is std + thiserror
//! only), we do the calendar math ourselves with Howard Hinnant's well-known
//! `civil_from_days` algorithm, which is exact for all proleptic-Gregorian
//! dates.

use std::time::SystemTime;

/// Return `(year, month, day, hour, minute)` in UTC for the given instant.
fn civil(t: SystemTime) -> (i64, u32, u32, u32, u32) {
    let secs = match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    };
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let hour = (sod / 3600) as u32;
    let minute = ((sod % 3600) / 60) as u32;
    (y, m, d, hour, minute)
}

/// Convert a count of days since the Unix epoch into `(year, month, day)`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let a = if z >= 0 { z } else { z - 146_096 };
    let era = a / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

/// Format an instant as `MM/DD/YY` (two-digit year, as the classic tools do).
pub(crate) fn date_mmddyy(t: SystemTime) -> String {
    let (y, m, d, _, _) = civil(t);
    format!("{:02}/{:02}/{:02}", m, d, y.rem_euclid(100))
}

/// Format an instant as `HH:MM` (24-hour, UTC).
pub(crate) fn time_hhmm(t: SystemTime) -> String {
    let (_, _, _, h, mi) = civil(t);
    format!("{h:02}:{mi:02}")
}
