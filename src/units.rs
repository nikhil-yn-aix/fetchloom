//! one way to write sizes, rates, durations and counts, everywhere fetchloom prints them.

use std::time::Duration;

/// a size in binary units: `512 B`, `4.1 KiB`, `3.4 GiB`.
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut unit = 0;
    let mut divisor: u128 = 1024;
    loop {
        let tenths = (u128::from(bytes) * 10 + divisor / 2) / divisor;
        if tenths < 10_240 || unit == UNITS.len() - 1 {
            return format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit]);
        }
        unit += 1;
        divisor *= 1024;
    }
}

/// a rate in binary units per second: `84.0 MiB/s`.
pub(crate) fn rate(bytes: u64, elapsed: Duration) -> String {
    let millis = elapsed.as_millis().max(1);
    let per_second = u64::try_from(u128::from(bytes) * 1000 / millis).unwrap_or(u64::MAX);
    size(per_second) + "/s"
}

/// a rate that is already in bytes per second, such as a moving average: `3.4 MiB/s`.
pub(crate) fn speed(per_second: f64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = if per_second.is_finite() {
        per_second.max(0.0)
    } else {
        0.0
    };
    if value < 1023.5 {
        return format!("{value:.0} B/s");
    }
    let mut unit = 0;
    value /= 1024.0;
    while value >= 1023.95 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}/s", UNITS[unit])
}

/// a duration: `41 ms` below a second, `3.2 s` below a minute, `4 min 05 s` above.
pub(crate) fn duration(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        return format!("{millis} ms");
    }
    let seconds = elapsed.as_secs_f64();
    if seconds < 60.0 {
        return format!("{seconds:.1} s");
    }
    let whole = elapsed.as_secs();
    format!("{} min {:02} s", whole / 60, whole % 60)
}

/// a duration in whole seconds, for clocks that tick on screen: `12 s`, `4 min 05 s`.
pub(crate) fn seconds(elapsed: Duration) -> String {
    let whole = elapsed.as_secs();
    if whole < 60 {
        format!("{whole} s")
    } else {
        format!("{} min {:02} s", whole / 60, whole % 60)
    }
}

/// a count with its noun: `1 file`, `1,204 files`, `2 retries`.
pub(crate) fn count(n: u64, one: &str, many: &str) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{grouped} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KiB");
        assert_eq!(size(4177), "4.1 KiB");
        assert_eq!(
            size(1_048_525),
            "1.0 MiB",
            "rounding up to 1024.0 KiB moves to the next unit"
        );
        assert_eq!(size(3_650_722_201), "3.4 GiB");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(Duration::from_millis(41)), "41 ms");
        assert_eq!(duration(Duration::from_millis(3240)), "3.2 s");
        assert_eq!(duration(Duration::from_secs(245)), "4 min 05 s");
    }

    #[test]
    fn counts() {
        assert_eq!(count(1, "file", "files"), "1 file");
        assert_eq!(count(0, "file", "files"), "0 files");
        assert_eq!(count(1204, "file", "files"), "1,204 files");
        assert_eq!(count(1_000_000, "retry", "retries"), "1,000,000 retries");
    }

    #[test]
    fn rates() {
        assert_eq!(rate(88_080_384, Duration::from_secs(1)), "84.0 MiB/s");
        assert_eq!(speed(512.0), "512 B/s");
        assert_eq!(speed(3_565_158.4), "3.4 MiB/s");
        assert_eq!(
            speed(1_048_566.0),
            "1.0 MiB/s",
            "1023.99 KiB rounds into the next unit"
        );
        assert_eq!(speed(1_048_524.0), "1023.9 KiB/s");
        assert_eq!(speed(f64::NAN), "0 B/s");
    }

    #[test]
    fn clocks_tick_in_whole_seconds() {
        assert_eq!(seconds(Duration::from_millis(12_900)), "12 s");
        assert_eq!(seconds(Duration::from_secs(245)), "4 min 05 s");
    }
}
