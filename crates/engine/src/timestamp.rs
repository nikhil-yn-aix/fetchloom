//! Wall clock instants, recorded but never used to decide anything.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// An instant written as a coordinated universal time stamp with second
/// precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Timestamp {
    seconds: i64,
}

impl Timestamp {
    /// Builds an instant from a count of seconds since the epoch.
    #[must_use]
    pub fn from_epoch_seconds(seconds: i64) -> Self {
        Self { seconds }
    }

    /// Reads the current wall clock.
    #[must_use]
    pub fn now() -> Self {
        let seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
            Err(before) => i64::try_from(before.duration().as_secs()).map_or(i64::MIN, |s| -s),
        };
        Self { seconds }
    }

    /// Returns the count of seconds since the epoch.
    #[must_use]
    pub fn epoch_seconds(self) -> i64 {
        self.seconds
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let days = self.seconds.div_euclid(86_400);
        let within_day = self.seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        let hour = within_day / 3_600;
        let minute = (within_day % 3_600) / 60;
        let second = within_day % 60;
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        )
    }
}

impl From<Timestamp> for String {
    fn from(timestamp: Timestamp) -> Self {
        timestamp.to_string()
    }
}

/// A timestamp string was not a coordinated universal time stamp with second
/// precision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseTimestampError;

impl fmt::Display for ParseTimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("timestamp is not of the form YYYY-MM-DDThh:mm:ssZ")
    }
}

impl std::error::Error for ParseTimestampError {}

impl std::str::FromStr for Timestamp {
    type Err = ParseTimestampError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
            return Err(ParseTimestampError);
        }
        if bytes[13] != b':' || bytes[16] != b':' || bytes[19] != b'Z' {
            return Err(ParseTimestampError);
        }
        let year = number(&text[0..4])?;
        let month = number(&text[5..7])?;
        let day = number(&text[8..10])?;
        let hour = number(&text[11..13])?;
        let minute = number(&text[14..16])?;
        let second = number(&text[17..19])?;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return Err(ParseTimestampError);
        }
        if hour > 23 || minute > 59 || second > 59 {
            return Err(ParseTimestampError);
        }
        let days = days_from_civil(year, month, day);
        Ok(Self {
            seconds: days * 86_400 + hour * 3_600 + minute * 60 + second,
        })
    }
}

impl TryFrom<String> for Timestamp {
    type Error = ParseTimestampError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

fn number(text: &str) -> Result<i64, ParseTimestampError> {
    text.parse().map_err(|_| ParseTimestampError)
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = shifted_month + if shifted_month < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}
