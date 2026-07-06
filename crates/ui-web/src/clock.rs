//! Wall-clock helpers for message timestamps.
//!
//! The arithmetic ([`utc_hhmm`]) is pure and host-tested; only reading the
//! browser's clock and timezone ([`now_ms`], [`local_hhmm`]) is wasm-gated,
//! with deterministic host stand-ins so reducers and tests stay DOM-free.

/// Milliseconds since the unix epoch, from the browser clock (0 on the host,
/// so host-built mocks stay deterministic).
pub fn now_ms() -> i64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as i64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// `HH:MM` for a unix-ms timestamp in UTC. Pure — host-tested below.
pub fn utc_hhmm(at_unix_ms: i64) -> String {
    let secs = at_unix_ms.div_euclid(1000);
    let of_day = secs.rem_euclid(86_400);
    format!("{:02}:{:02}", of_day / 3600, (of_day % 3600) / 60)
}

/// `HH:MM` for a unix-ms timestamp in the reader's local timezone (browser
/// clock; falls back to UTC on the host, where there is no locale).
pub fn local_hhmm(at_unix_ms: i64) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at_unix_ms as f64));
        format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        utc_hhmm(at_unix_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_time_of_day() {
        // 2026-07-06 14:35:07 UTC.
        assert_eq!(utc_hhmm(1_783_780_507_000), "14:35");
    }

    #[test]
    fn midnight_and_end_of_day_pad_correctly() {
        assert_eq!(utc_hhmm(0), "00:00");
        assert_eq!(utc_hhmm(86_399_000), "23:59");
    }

    #[test]
    fn pre_epoch_times_stay_in_range() {
        // rem_euclid keeps negative timestamps a valid time of day.
        assert_eq!(utc_hhmm(-60_000), "23:59");
    }
}
