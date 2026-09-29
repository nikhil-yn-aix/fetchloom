//! Points in time as lock files record them.

use std::fmt;
use std::str::FromStr;

/// A UTC time to the second in RFC 3339 form, `2026-09-28T10:00:00Z`, on a real calendar date.
///
/// It is only ever read and written, never taken from a clock here.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Timestamp(String);

impl Timestamp {
    /// The time as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for Timestamp {
    type Err = TimestampError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if valid(text.as_bytes()) {
            Ok(Self(text.to_owned()))
        } else {
            Err(TimestampError(text.to_owned()))
        }
    }
}

const SHAPE: &[u8; 20] = b"0000-00-00T00:00:00Z";

fn valid(bytes: &[u8]) -> bool {
    let shaped = bytes.len() == SHAPE.len()
        && bytes.iter().zip(SHAPE).all(|(byte, shape)| match shape {
            b'0' => byte.is_ascii_digit(),
            _ => byte == shape,
        });
    if !shaped {
        return false;
    }
    let field = |start: usize, len: usize| {
        bytes[start..start + len]
            .iter()
            .fold(0_u32, |value, digit| value * 10 + u32::from(digit - b'0'))
    };
    let (year, month, day) = (field(0, 4), field(5, 2), field(8, 2));
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && field(11, 2) < 24 && field(14, 2) < 60 && field(17, 2) < 60
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Text that is not a UTC time to the second.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a UTC time like `2026-09-28T10:00:00Z`")]
pub struct TimestampError(String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_utc_seconds_on_real_dates() {
        for text in [
            "2026-09-28T10:00:00Z",
            "2024-02-29T23:59:59Z",
            "2026-02-28T10:00:00Z",
            "2000-02-29T00:00:00Z",
            "1970-01-01T00:00:00Z",
            "9999-12-31T23:59:59Z",
        ] {
            let timestamp: Timestamp = text.parse().unwrap();
            assert_eq!(timestamp.as_str(), text);
            assert_eq!(timestamp.to_string(), text);
        }
    }

    #[test]
    fn refuses_anything_else() {
        for text in [
            "",
            "2026-09-28",
            "2026-09-28T10:00:00",
            "2026-09-28T10:00:00+00:00",
            "2026-09-28T10:00:00.5Z",
            "2026-09-28 10:00:00Z",
            "2026-09-28t10:00:00z",
            "2026-9-28T10:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-01-00T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "1900-02-29T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:60:00Z",
            "2026-01-01T00:00:60Z",
            "20a6-01-01T00:00:00Z",
            "2026-01-01T00:00:0\u{e9}",
        ] {
            let err = text.parse::<Timestamp>().unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("`{text}` is not a UTC time like `2026-09-28T10:00:00Z`"),
            );
        }
    }
}
