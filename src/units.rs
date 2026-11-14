//! one way to write sizes, rates, durations, counts, lists and paths, everywhere fetchloom prints
//! them.

use std::path::{MAIN_SEPARATOR, Path};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

/// what downloads brought, as `79.6 MiB in 3.2 s · 17.7 MiB/s · 23.0 MiB from an earlier run`. the
/// rate counts only bytes this run moved, until the last arrived, so a resume or a slow disk never
/// skews it; a disk that kept the files a second or more after is named, with the whole time.
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

/// an estimate as a person plans around it, rounded to what it can claim: `under 10 s`,
/// `about 40 s`, `about 3 min`, `about 15 min`, `about 1 h 30 min`, `about 14 h`, `about 3 days`.
pub(crate) fn about(estimate: Duration) -> String {
    let seconds = estimate.as_secs_f64();
    if seconds < 10.0 {
        return "under 10 s".to_owned();
    }
    format!("about {}", rough(seconds, f64::round))
}

/// a floor in the steps of [`about`], rounded down so it never claims more: `5 min`, `1 h`.
pub(crate) fn at_least(floor: Duration) -> String {
    rough(floor.as_secs_f64(), f64::floor)
}

/// `seconds` in a step that grows with it, rounded by `round`: tens of seconds, minutes, five
/// minutes, half hours, hours, then days.
fn rough(seconds: f64, round: fn(f64) -> f64) -> String {
    const MINUTE: f64 = 60.0;
    const HOUR: f64 = 3600.0;
    let step = |seconds: f64, step: f64| round(seconds / step) * step;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "whole steps of a duration"
    )]
    let whole = |value: f64| value as u64;
    let (tens, minutes, fives, halves) = (
        step(seconds, 10.0),
        step(seconds, MINUTE),
        step(seconds, 5.0 * MINUTE),
        step(seconds, HOUR / 2.0),
    );
    if tens < MINUTE {
        return format!("{} s", whole(tens).max(10));
    }
    if minutes < 10.0 * MINUTE {
        return format!("{} min", whole(minutes / MINUTE));
    }
    if fives < HOUR {
        return format!("{} min", whole(fives / MINUTE));
    }
    if halves < 10.0 * HOUR {
        let halves = whole(halves / (HOUR / 2.0));
        return match halves % 2 {
            0 => format!("{} h", halves / 2),
            _ => format!("{} h 30 min", halves / 2),
        };
    }
    let hours = step(seconds, HOUR);
    if hours < 48.0 * HOUR {
        return format!("{} h", whole(hours / HOUR));
    }
    format!("{} days", whole(step(seconds, 24.0 * HOUR) / (24.0 * HOUR)))
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

/// the columns plain `text` takes on a terminal: two for most chinese, japanese and korean
/// characters, none for a mark that combines with the one before it.
pub(crate) fn columns(text: &str) -> usize {
    text.width()
}

/// the longest start of `text` at most `most` columns wide.
pub(crate) fn within(text: &str, most: usize) -> &str {
    let mut taken = 0;
    let end = text
        .char_indices()
        .find(|(_, c)| {
            taken += c.width().unwrap_or(0);
            taken > most
        })
        .map_or(text.len(), |(end, _)| end);
    &text[..end]
}

/// free text on one line and at most `most` columns wide, cut after a whole word and ended with
/// `more` when it is longer.
pub(crate) fn clip(text: &str, most: usize, more: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let head = within(&flat, most);
    let end = head.len();
    if end == flat.len() {
        return flat;
    }
    let kept = match head.rfind(' ') {
        Some(space)
            if !flat[end..].starts_with(' ')
                && columns(&head[..space]) >= columns(head) * 2 / 3 =>
        {
            &head[..space]
        }
        _ => head,
    };
    // a cut after a comma or a colon reads better without it
    let kept = kept.trim_end_matches([',', ';', ':', ' ']);
    format!("{kept}{more}")
}

/// `text` at most `most` columns wide, with its middle cut out for `more`, so both ends stay, as
/// a link's site and its file do.
pub(crate) fn clip_middle(text: &str, most: usize, more: &str) -> String {
    if columns(text) <= most {
        return text.to_owned();
    }
    let room = most.saturating_sub(columns(more));
    let head = within(text, room / 2);
    let wanted = room - columns(head);
    let mut tail = "";
    for (at, _) in text.char_indices().rev() {
        if columns(&text[at..]) > wanted {
            break;
        }
        tail = &text[at..];
    }
    format!("{head}{more}{tail}")
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
    // carry decimal digits from the right so rounding needs no precision-sized integer
    let fraction = fraction.bytes().rev().fold(0, |tail, digit| {
        (tail + u64::from(digit - b'0') * multiplier) / 10
    });
    whole.checked_add(fraction).filter(|bytes| *bytes > 0)
}

/// `now` in UTC to the second, as RFC 3339 writes it and data.lock records it:
/// `2026-10-27T17:30:00Z`.
pub(crate) fn timestamp(now: SystemTime) -> String {
    let seconds = now
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    // days since 1970 to a civil date, as in Howard Hinnant's chrono algorithms
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// `time` as the project state and the cache record times: nanoseconds since 1970, zero before
/// then, and capped at the largest unsigned 64-bit integer.
pub(crate) fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the home folder alone is `~`, and a path elsewhere, or a home of `/`, stays whole.
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

    /// an estimate rounds to a step that grows with it, and a floor rounds down, never up.
    #[test]
    fn estimates_round_to_what_they_can_claim() {
        let about = |seconds: u64| about(Duration::from_secs(seconds));
        for (seconds, said) in [
            (0, "under 10 s"),
            (9, "under 10 s"),
            (10, "about 10 s"),
            (44, "about 40 s"),
            (55, "about 1 min"),
            (185, "about 3 min"),
            (570, "about 10 min"),
            (1_000, "about 15 min"),
            (3_450, "about 1 h"),
            (5_400, "about 1 h 30 min"),
            (35_100, "about 10 h"),
            (50_000, "about 14 h"),
            (259_200, "about 3 days"),
        ] {
            assert_eq!(about(seconds), said, "{seconds} s");
        }
        let at_least = |seconds: u64| at_least(Duration::from_secs(seconds));
        assert_eq!(at_least(300), "5 min");
        assert_eq!(at_least(599), "9 min");
        assert_eq!(at_least(3_599), "55 min");
        assert_eq!(at_least(7_199), "1 h 30 min");
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

    /// the rest's count groups thousands, as 1,201, and a short list is listed whole.
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

    /// the disk's share of the time comes beside the whole wait, and the rate stays the link's.
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
        assert_eq!(
            clip("鸢尾花数据集的测量结果", 10, "…"),
            "鸢尾花数据…",
            "a wide character takes two columns"
        );
        assert_eq!(
            clip("ab鸢尾花", 5, "..."),
            "ab鸢...",
            "a wide character that would cross the last column goes"
        );
    }

    #[test]
    #[expect(
        clippy::duration_suboptimal_units,
        reason = "unix timestamps count seconds"
    )]
    fn timestamps_are_utc_and_exact() {
        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_793_122_200)),
            "2026-10-27T17:30:00Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(951_782_400)),
            "2000-02-29T00:00:00Z"
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
