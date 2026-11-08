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

/// what downloads brought, from each file's size and the bytes this run moved for it:
/// `79.6 MiB in 3.2 s · 17.7 MiB/s · 23.0 MiB from an earlier run`. the rate counts only the
/// bytes moved, so a resumed file does not look faster than the link.
pub(crate) fn downloaded(files: impl IntoIterator<Item = (u64, u64)>, elapsed: Duration) -> String {
    let (mut total, mut moved, mut earlier) = (0_u64, 0_u64, 0_u64);
    for (file, moved_for_it) in files {
        total = total.saturating_add(file);
        moved = moved.saturating_add(moved_for_it);
        earlier = earlier.saturating_add(file.saturating_sub(moved_for_it));
    }
    let mut text = format!("{} in {}", size(total), duration(elapsed));
    if elapsed >= Duration::from_secs(1) {
        text.push_str(" · ");
        text.push_str(&rate(moved, elapsed));
    }
    if earlier > 0 {
        text.push_str(" · ");
        text.push_str(&size(earlier));
        text.push_str(" from an earlier run");
    }
    text
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

/// a duration: `41 ms` below a second, `3.2 s` below a minute, `4 min 05 s` above. a time that
/// rounds to a whole minute is told in minutes.
pub(crate) fn duration(elapsed: Duration) -> String {
    let millis = elapsed.as_millis();
    if millis < 1000 {
        return format!("{millis} ms");
    }
    let seconds = elapsed.as_secs_f64();
    if seconds < 59.95 {
        return format!("{seconds:.1} s");
    }
    let whole = elapsed.as_secs().max(60);
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

/// free text on one line and at most `most` characters, cut after a whole word and ended with
/// `more` when it is longer.
pub(crate) fn clip(text: &str, most: usize, more: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let Some((end, _)) = flat.char_indices().nth(most) else {
        return flat;
    };
    let head = &flat[..end];
    let kept = match head.rfind(' ') {
        Some(space) if !flat[end..].starts_with(' ') && space >= head.len() * 2 / 3 => {
            &head[..space]
        }
        _ => head,
    };
    // a cut after a comma or a colon reads better without it
    let kept = kept.trim_end_matches([',', ';', ':', ' ']);
    format!("{kept}{more}")
}

/// a compact decimal count for catalog popularity.
pub(crate) fn compact(n: u64) -> String {
    const UNITS: [&str; 6] = ["k", "M", "B", "T", "P", "E"];
    if n < 1000 {
        return n.to_string();
    }
    let n = u128::from(n);
    let mut divisor = 1000;
    let mut unit = 0;
    while (n + divisor / 2) / divisor >= 1000 && unit < UNITS.len() - 1 {
        divisor *= 1000;
        unit += 1;
    }
    let tenths = (n * 10 + divisor / 2) / divisor;
    if tenths < 100 && tenths % 10 != 0 {
        format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit])
    } else {
        format!("{}{}", (n + divisor / 2) / divisor, UNITS[unit])
    }
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
        assert_eq!(duration(Duration::from_millis(59_949)), "59.9 s");
        for rounded_up in [59_950, 59_999] {
            assert_eq!(
                duration(Duration::from_millis(rounded_up)),
                "1 min 00 s",
                "{rounded_up} ms rounds into the next unit"
            );
        }
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

    #[test]
    fn a_download_rates_only_the_bytes_this_run_moved() {
        const MIB: u64 = 1024 * 1024;
        assert_eq!(
            downloaded([(80 * MIB, 80 * MIB)], Duration::from_secs(2)),
            "80.0 MiB in 2.0 s · 40.0 MiB/s"
        );
        assert_eq!(
            downloaded([(80 * MIB, 80 * MIB)], Duration::from_millis(803)),
            "80.0 MiB in 803 ms",
            "below a second a rate would only be noise"
        );
        assert_eq!(
            downloaded(
                [(40 * MIB, 10 * MIB), (40 * MIB, 40 * MIB)],
                Duration::from_secs(2)
            ),
            "80.0 MiB in 2.0 s · 25.0 MiB/s · 30.0 MiB from an earlier run",
            "an earlier run's bytes count in the size, not in the rate"
        );
        assert_eq!(
            downloaded(
                [(40 * MIB, 10 * MIB), (40 * MIB, 60 * MIB)],
                Duration::from_secs(2)
            ),
            "80.0 MiB in 2.0 s · 35.0 MiB/s · 30.0 MiB from an earlier run",
            "a file fetched twice does not hide another's kept bytes"
        );
    }

    #[test]
    fn clipped_text_keeps_one_line_and_whole_words() {
        assert_eq!(clip("DatasetIris", 50, "…"), "DatasetIris");
        assert_eq!(clip(" Iris\n\tdata  set ", 50, "…"), "Iris data set");
        assert_eq!(
            clip(
                "Effects of xylooligosaccharide supplementation on methane emissions",
                50,
                "…"
            ),
            "Effects of xylooligosaccharide supplementation on…"
        );
        assert_eq!(
            clip("one two three", 7, "..."),
            "one two...",
            "a cut that lands between words keeps the last one"
        );
        assert_eq!(
            clip(&"é".repeat(60), 50, "…"),
            format!("{}…", "é".repeat(50))
        );
        assert_eq!(
            clip(
                "Data files © Original Authors, as the description says",
                32,
                "…"
            ),
            "Data files © Original Authors…",
            "a comma before the cut goes with it"
        );
    }
}

#[cfg(test)]
mod compact_tests {
    use super::compact;
    #[test]
    fn compact_counts_round_without_losing_the_unit() {
        for (n, expected) in [
            (0, "0"),
            (950, "950"),
            (999, "999"),
            (1_000, "1k"),
            (1_200, "1.2k"),
            (12_000, "12k"),
            (12_490, "12k"),
            (999_500, "1M"),
            (3_400_000, "3.4M"),
            (1_000_000_000, "1B"),
            (u64::MAX, "18E"),
        ] {
            assert_eq!(compact(n), expected, "{n}");
        }
    }
}
