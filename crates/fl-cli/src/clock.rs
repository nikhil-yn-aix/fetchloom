//! The present moment as lock files record it.

use std::time::{SystemTime, UNIX_EPOCH};

use fl_model::error::ErrorKind;
use fl_model::timestamp::Timestamp;

use crate::failure::Failure;

/// Now, in UTC to the second.
pub(crate) fn now() -> Result<Timestamp, Failure> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    at(seconds).parse().map_err(|_| {
        Failure::new(
            ErrorKind::LocalIo,
            "the system clock is set past the year 9999",
        )
    })
}

/// The moment `seconds` after the unix epoch, in RFC 3339.
fn at(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let rest = seconds % 86_400;
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The proleptic Gregorian date `days` after 1970-01-01, by Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * shifted + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    })
    .unwrap_or(1);
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_moments_are_written_in_rfc_3339() {
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_790_590_000), "2026-09-28T10:06:40Z");
    }

    #[test]
    fn century_and_era_boundaries_land_on_the_right_day() {
        for (seconds, day) in [
            (4_107_456_000, "2100-02-28"),
            (4_107_542_400, "2100-03-01"),
            (7_289_568_000, "2200-12-31"),
            (13_569_379_200, "2399-12-31"),
            (13_574_563_200, "2400-02-29"),
            (13_574_649_600, "2400-03-01"),
        ] {
            assert_eq!(at(seconds), format!("{day}T00:00:00Z"));
        }
    }
}
