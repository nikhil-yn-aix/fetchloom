//! one way to write sizes, rates, durations, counts, lists and paths, everywhere fetchloom prints
//! them.

use std::path::{MAIN_SEPARATOR, Path};
use std::time::Duration;

/// `path` as a line a person reads shows it: below the home folder as `~/…`, else whole. a
/// command to run keeps the whole path, since a shell reads no `~` inside quotes.
pub(crate) fn path(path: &Path) -> String {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" });
    below_home(path, home.as_deref().map(Path::new))
}

/// `path` with the home folder `home` written `~`.
fn below_home(path: &Path, home: Option<&Path>) -> String {
    let inside = home
        .filter(|home| home.parent().is_some())
        .and_then(|home| path.strip_prefix(home).ok());
    match inside {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~{MAIN_SEPARATOR}{}", rest.display()),
        None => path.display().to_string(),
    }
}

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

/// a rate in binary units per second: `84.0 MiB/s`, or `<1 B/s` for bytes that came slower than
/// one a second, which is not no rate at all.
pub(crate) fn rate(bytes: u64, elapsed: Duration) -> String {
    let millis = elapsed.as_millis().max(1);
    let per_second = u64::try_from(u128::from(bytes) * 1000 / millis).unwrap_or(u64::MAX);
    if bytes > 0 && per_second == 0 {
        return TRICKLE.to_owned();
    }
    size(per_second) + "/s"
}

/// a rate above zero but below one byte a second.
const TRICKLE: &str = "<1 B/s";

/// how long downloads took: until their last byte `arrived`, and `all` of it, the time the disk
/// took to keep them included, as `Fetched::transfer` measures both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Took {
    pub(crate) arrived: Duration,
    pub(crate) all: Duration,
}

impl From<Duration> for Took {
    fn from(elapsed: Duration) -> Took {
        Took {
            arrived: elapsed,
            all: elapsed,
        }
    }
}

/// what downloads brought, from each file's size and the bytes this run moved for it:
/// `79.6 MiB in 3.2 s · 17.7 MiB/s · 23.0 MiB from an earlier run`. the rate counts only the
/// bytes moved, so a resumed file does not look faster than the link, and only the time until
/// the last byte arrived, so a slow disk does not make the link look slow either. a disk that
/// took a second or more to keep the files after that is named with the whole time:
/// `1.0 GiB in 30.2 s, 12.3 s of it waiting for the disk · 57.2 MiB/s`.
pub(crate) fn downloaded(
    files: impl IntoIterator<Item = (u64, u64)>,
    took: impl Into<Took>,
) -> String {
    let Took { arrived, all } = took.into();
    let (mut total, mut moved, mut earlier) = (0_u64, 0_u64, 0_u64);
    for (file, moved_for_it) in files {
        total = total.saturating_add(file);
        moved = moved.saturating_add(moved_for_it);
        earlier = earlier.saturating_add(file.saturating_sub(moved_for_it));
    }
    let disk = all.saturating_sub(arrived);
    let mut text = if disk >= Duration::from_secs(1) {
        format!(
            "{} in {}, {} of it waiting for the disk",
            size(total),
            duration(all),
            duration(disk)
        )
    } else {
        format!("{} in {}", size(total), duration(arrived))
    };
    if arrived >= Duration::from_secs(1) {
        text.push_str(" · ");
        text.push_str(&rate(moved, arrived));
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
    if value > 0.0 && value < 1.0 {
        return TRICKLE.to_owned();
    }
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
    format!("{} {}", grouped(n), if n == 1 { one } else { many })
}

/// a number with its thousands apart: `1,204`.
pub(crate) fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `words` as a sentence lists them, `last` before the last one: `a`, `a and b`, `a, b and c`,
/// or `a, b or c` for a choice.
pub(crate) fn listed(words: &[impl AsRef<str>], last: &str) -> String {
    let words: Vec<&str> = words.iter().map(AsRef::as_ref).collect();
    match words.split_last() {
        None => String::new(),
        Some((only, [])) => (*only).to_owned(),
        Some((final_word, rest)) => format!("{} {last} {final_word}", rest.join(", ")),
    }
}

/// the first `most` of `words` and how many more there are, `a, b, c and 2 more`, or all of
/// them [`listed`] when they are no more than `most`.
pub(crate) fn some_of(words: &[impl AsRef<str>], most: usize) -> String {
    if words.len() <= most {
        return listed(words, "and");
    }
    let shown: Vec<&str> = words[..most].iter().map(AsRef::as_ref).collect();
    format!(
        "{} and {} more",
        shown.join(", "),
        grouped((words.len() - most) as u64)
    )
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

/// a positive byte count, optionally with a binary suffix `K`, `M`, `G` or `T`, as `500K` or
/// `2.5M`. surrounding whitespace is accepted.
pub(crate) fn bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, power) = match text.as_bytes().last()? {
        b'K' => (&text[..text.len() - 1], 1),
        b'M' => (&text[..text.len() - 1], 2),
        b'G' => (&text[..text.len() - 1], 3),
        b'T' => (&text[..text.len() - 1], 4),
        _ => (text, 0),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if !digits(whole) || (number.contains('.') && !digits(fraction)) {
        return None;
    }
    let multiplier = 1024_u64.pow(power);
    let whole = whole.parse::<u64>().ok()?.checked_mul(multiplier)?;
    // carry decimal digits from the right so rounding needs no precision-sized integer.
    let fraction = fraction.bytes().rev().fold(0, |tail, digit| {
        (tail + u64::from(digit - b'0') * multiplier) / 10
    });
    whole.checked_add(fraction).filter(|bytes| *bytes > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a path below the home folder reads from `~`; the home folder alone is `~`, and a path
    /// elsewhere, or a home of `/`, stays whole.
    #[cfg(unix)]
    #[test]
    fn a_path_below_home_reads_from_the_tilde() {
        let home = Some(Path::new("/home/ada"));
        for (path, shown) in [
            (
                "/home/ada/.config/fetchloom/settings.toml",
                "~/.config/fetchloom/settings.toml",
            ),
            ("/home/ada", "~"),
            ("/home/adam/data", "/home/adam/data"),
            ("/scratch/lab", "/scratch/lab"),
        ] {
            assert_eq!(below_home(Path::new(path), home), shown, "{path}");
        }
        assert_eq!(
            below_home(Path::new("/srv/x"), Some(Path::new("/"))),
            "/srv/x"
        );
        assert_eq!(below_home(Path::new("/srv/x"), None), "/srv/x");
    }

    #[test]
    fn sizes_round_to_a_tenth_and_move_up_a_unit_at_1024() {
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
    fn durations_go_from_ms_to_tenths_of_a_second_to_minutes() {
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
    fn counts_pick_one_or_many_and_group_thousands() {
        assert_eq!(count(1, "file", "files"), "1 file");
        assert_eq!(count(0, "file", "files"), "0 files");
        assert_eq!(count(1204, "file", "files"), "1,204 files");
        assert_eq!(count(1_000_000, "retry", "retries"), "1,000,000 retries");
    }

    /// a list reads as a sentence: commas between the words, and `and` or `or` before the last.
    #[test]
    fn lists_read_as_a_sentence() {
        let cases: [(&[&str], &str, &str); 6] = [
            (&[], "and", ""),
            (&["a"], "and", "a"),
            (&["a", "b"], "and", "a and b"),
            (&["a", "b", "c"], "and", "a, b and c"),
            (&["a", "b"], "or", "a or b"),
            (&["a", "b", "c", "d"], "or", "a, b, c or d"),
        ];
        for (words, last, said) in cases {
            assert_eq!(listed(words, last), said, "{words:?} with {last}");
        }
    }

    /// a long list names its first few and counts the rest with thousands apart, and a short one
    /// is listed whole.
    #[test]
    fn long_lists_name_a_few_and_count_the_rest() {
        let words: Vec<String> = (1..=1_204).map(|n| n.to_string()).collect();
        let cases: [(usize, &str); 5] = [
            (0, ""),
            (1, "1"),
            (3, "1, 2 and 3"),
            (4, "1, 2, 3 and 1 more"),
            (1_204, "1, 2, 3 and 1,201 more"),
        ];
        for (given, said) in cases {
            assert_eq!(some_of(&words[..given], 3), said, "{given} words");
        }
    }

    #[test]
    fn rates_round_like_sizes_and_nan_reads_as_zero() {
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
    fn a_slow_trickle_is_never_told_as_no_rate() {
        assert_eq!(rate(12, Duration::from_millis(12_050)), "<1 B/s");
        assert_eq!(speed(0.4), "<1 B/s");
        assert_eq!(rate(0, Duration::from_secs(12)), "0 B/s");
        assert_eq!(speed(0.0), "0 B/s");
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

    /// a closing line says the whole time a person waited, naming the part the disk took to keep
    /// the files once their last byte arrived, while the rate stays the link's.
    #[test]
    fn a_download_names_the_time_its_disk_took() {
        const MIB: u64 = 1024 * 1024;
        let took = |arrived: u64, all: u64| Took {
            arrived: Duration::from_millis(arrived),
            all: Duration::from_millis(all),
        };
        assert_eq!(
            downloaded([(1024 * MIB, 1024 * MIB)], took(17_900, 30_200)),
            "1.0 GiB in 30.2 s, 12.3 s of it waiting for the disk · 57.2 MiB/s"
        );
        assert_eq!(
            downloaded([(20 * MIB, 20 * MIB)], took(22, 14_800)),
            "20.0 MiB in 14.8 s, 14.8 s of it waiting for the disk"
        );
        assert_eq!(
            downloaded([(80 * MIB, 80 * MIB)], took(2_000, 2_600)),
            "80.0 MiB in 2.0 s · 40.0 MiB/s",
            "a flush under a second is no wait worth a word"
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
