use super::*;

#[test]
fn known_epoch_seconds_format_correctly() {
    // 2024-03-05T06:07:08Z
    assert_eq!(format_for_test(1709618828), "2024-03-05T06:07:08Z");
}

#[test]
fn epoch_zero_formats_as_epoch_start() {
    assert_eq!(format_for_test(0), "1970-01-01T00:00:00Z");
}

#[test]
fn leap_day_formats_correctly() {
    // 2024-02-29T12:00:00Z
    assert_eq!(format_for_test(1709208000), "2024-02-29T12:00:00Z");
}

fn format_for_test(secs: u64) -> String {
    let (year, month, day, hour, min, sec) = decompose_epoch(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}
