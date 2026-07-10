//! UTC timestamp formatting.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current UTC time as an ISO 8601 timestamp, e.g. `2026-07-10T12:34:56Z`.
pub fn utc_now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day, hour, min, sec) = decompose_epoch(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn decompose_epoch(secs: u64) -> (u32, u32, u64, u64, u64, u64) {
    let sec = secs % 60;
    let min = (secs / 60) % 60;
    let hour = (secs / 3600) % 24;
    let mut days = secs / 86400;

    let mut year = 1970u32;
    loop {
        let diy = days_in_year(year);
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }

    let dim = month_days(year);
    let mut month = 1u32;
    for &d in &dim {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }

    (year, month, days + 1, hour, min, sec)
}

fn days_in_year(year: u32) -> u64 {
    if is_leap_year(year) { 366 } else { 365 }
}

fn month_days(year: u32) -> [u64; 12] {
    [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ]
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
