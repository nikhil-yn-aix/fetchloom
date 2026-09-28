//! One way to write and read sizes, rates, durations and counts.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};

const BINARY: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// A number of bytes written for people: `512 B`, `3.4 GiB`, `84 MiB`.
///
/// Binary units, one decimal below 10 and none from 10 up, the unit chosen after rounding.
#[must_use]
pub fn size(bytes: u64) -> impl fmt::Display {
    Bytes { bytes, suffix: "" }
}

/// A rate in bytes per second written for people: `84 MiB/s`.
#[must_use]
pub fn rate(bytes_per_second: u64) -> impl fmt::Display {
    Bytes {
        bytes: bytes_per_second,
        suffix: "/s",
    }
}

/// A duration written for people: `41ms`, `1.2s`, `16s`, `2m 5s`, `1h 3m`.
#[must_use]
pub fn duration(elapsed: Duration) -> impl fmt::Display {
    Elapsed(elapsed)
}

/// A count with thousands separators and a noun: `1 file`, `1,204 files`.
#[must_use]
pub fn count<'a>(n: u64, one: &'a str, many: &'a str) -> impl fmt::Display + 'a {
    Count { n, one, many }
}

struct Bytes {
    bytes: u64,
    suffix: &'static str,
}

impl fmt::Display for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { bytes, suffix } = *self;
        if bytes < 1024 {
            return write!(f, "{bytes} B{suffix}");
        }
        let bytes = u128::from(bytes);
        let mut unit = 1_u128;
        for name in BINARY {
            unit *= 1024;
            let tenths = rounded(bytes * 10, unit);
            if tenths < 100 {
                return write!(f, "{}.{} {name}{suffix}", tenths / 10, tenths % 10);
            }
            let whole = rounded(bytes, unit);
            if whole < 1024 || name == "EiB" {
                return write!(f, "{whole} {name}{suffix}");
            }
        }
        Ok(())
    }
}

fn rounded(value: u128, unit: u128) -> u128 {
    (value + unit / 2) / unit
}

struct Elapsed(Duration);

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let millis = self.0.as_millis();
        if millis < 1000 {
            return write!(f, "{millis}ms");
        }
        let tenths = rounded(millis, 100);
        if tenths < 100 {
            return write!(f, "{}.{}s", tenths / 10, tenths % 10);
        }
        let seconds = rounded(millis, 1000);
        match seconds {
            0..60 => write!(f, "{seconds}s"),
            60..3600 => write!(f, "{}m {}s", seconds / 60, seconds % 60),
            _ => write!(f, "{}h {}m", seconds / 3600, seconds % 3600 / 60),
        }
    }
}

struct Count<'a> {
    n: u64,
    one: &'a str,
    many: &'a str,
}

impl fmt::Display for Count<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.n.to_string();
        for (i, digit) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                f.write_str(",")?;
            }
            write!(f, "{digit}")?;
        }
        let noun = if self.n == 1 { self.one } else { self.many };
        write!(f, " {noun}")
    }
}

/// A size limit as written in `data.toml`, such as `2 GiB` or `500 MB`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Size(u64);

impl Size {
    /// The size in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.0
    }
}

const UNITS: [(&str, u64); 9] = [
    ("B", 1),
    ("KB", 1000),
    ("MB", 1000 * 1000),
    ("GB", 1000 * 1000 * 1000),
    ("TB", 1000 * 1000 * 1000 * 1000),
    ("KiB", 1 << 10),
    ("MiB", 1 << 20),
    ("GiB", 1 << 30),
    ("TiB", 1 << 40),
];

impl FromStr for Size {
    type Err = SizeError;

    /// Reads a whole or decimal number, optional spaces, then an optional unit: `B`, `KB`, `MB`,
    /// `GB`, `TB` (powers of 1000) or `KiB`, `MiB`, `GiB`, `TiB` (powers of 1024), written as
    /// shown or all lowercase. A fraction of a byte is dropped.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || SizeError::Invalid(text.to_owned());
        let number_end = text
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(text.len());
        let (number, unit) = text.split_at(number_end);
        let unit = unit.trim_start_matches(' ');
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let digits_ok = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        if !digits_ok(whole) || (number.contains('.') && !digits_ok(fraction)) {
            return Err(invalid());
        }
        let multiplier = if unit.is_empty() {
            1
        } else {
            UNITS
                .iter()
                .find(|(name, _)| *name == unit || name.to_lowercase() == unit)
                .map(|&(_, multiplier)| multiplier)
                .ok_or_else(invalid)?
        };
        scale(whole, fraction, multiplier)
            .map(Self)
            .ok_or_else(|| SizeError::TooLarge(text.to_owned()))
    }
}

fn scale(whole: &str, fraction: &str, multiplier: u64) -> Option<u64> {
    let multiplier = u128::from(multiplier);
    let mut bytes = parse_digits(whole)?.checked_mul(multiplier)?;
    let mut denominator = 1_u128;
    let mut numerator = 0_u128;
    for digit in fraction.bytes().take(20) {
        denominator *= 10;
        numerator = numerator * 10 + u128::from(digit - b'0');
    }
    bytes = bytes.checked_add(numerator * multiplier / denominator)?;
    u64::try_from(bytes).ok()
}

fn parse_digits(digits: &str) -> Option<u128> {
    digits.bytes().try_fold(0_u128, |value, digit| {
        value.checked_mul(10)?.checked_add(u128::from(digit - b'0'))
    })
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SizeVisitor)
    }
}

struct SizeVisitor;

impl Visitor<'_> for SizeVisitor {
    type Value = Size;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a size like \"2 GiB\" or a number of bytes")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Size, E> {
        text.parse().map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, bytes: u64) -> Result<Size, E> {
        Ok(Size(bytes))
    }

    fn visit_i64<E: de::Error>(self, bytes: i64) -> Result<Size, E> {
        u64::try_from(bytes)
            .map(Size)
            .map_err(|_| E::custom("a size cannot be negative"))
    }
}

/// Why text is not a size.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SizeError {
    /// The text is not a number with an optional unit.
    #[error("`{0}` is not a size like `2 GiB` or `500 MB`")]
    Invalid(String),
    /// The size does not fit in 64 bits.
    #[error("size `{0}` is too large")]
    TooLarge(String),
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde::de::IntoDeserializer;
    use serde::de::value::Error as ValueError;

    use super::*;

    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    #[test]
    fn formats_sizes_in_binary_units_with_one_decimal_below_ten() {
        let cases = [
            (0, "0 B"),
            (1, "1 B"),
            (1023, "1023 B"),
            (KIB, "1.0 KiB"),
            (KIB + 51, "1.0 KiB"),
            (KIB + 52, "1.1 KiB"),
            (10 * KIB - 52, "9.9 KiB"),
            (10 * KIB - 51, "10 KiB"),
            (84 * MIB, "84 MiB"),
            (1_048_535, "1.0 MiB"),
            (1023 * KIB, "1023 KiB"),
            (3_650_722_202, "3.4 GiB"),
            (2 * GIB + GIB / 10, "2.1 GiB"),
            (u64::MAX, "16 EiB"),
        ];
        for (bytes, text) in cases {
            assert_eq!(size(bytes).to_string(), text, "{bytes}");
        }
    }

    #[test]
    fn formats_rates_as_sizes_per_second() {
        assert_eq!(rate(84 * MIB).to_string(), "84 MiB/s");
        assert_eq!(rate(512).to_string(), "512 B/s");
    }

    #[test]
    fn formats_durations_by_their_magnitude() {
        let cases = [
            (0, "0ms"),
            (41, "41ms"),
            (999, "999ms"),
            (1000, "1.0s"),
            (1249, "1.2s"),
            (1250, "1.3s"),
            (9949, "9.9s"),
            (9950, "10s"),
            (16_000, "16s"),
            (59_499, "59s"),
            (59_500, "1m 0s"),
            (125_000, "2m 5s"),
            (3_599_000, "59m 59s"),
            (3_599_500, "1h 0m"),
            (3_780_000, "1h 3m"),
            (90_000_000, "25h 0m"),
        ];
        for (millis, text) in cases {
            assert_eq!(
                duration(Duration::from_millis(millis)).to_string(),
                text,
                "{millis}"
            );
        }
        assert_eq!(duration(Duration::from_micros(41_999)).to_string(), "41ms");
    }

    #[test]
    fn formats_counts_with_separators_and_a_noun() {
        let cases = [
            (0, "0 files"),
            (1, "1 file"),
            (2, "2 files"),
            (999, "999 files"),
            (1204, "1,204 files"),
            (1_000_000, "1,000,000 files"),
            (u64::MAX, "18,446,744,073,709,551,615 files"),
        ];
        for (n, text) in cases {
            assert_eq!(count(n, "file", "files").to_string(), text);
        }
        assert_eq!(count(3, "entry", "entries").to_string(), "3 entries");
    }

    #[test]
    fn parses_sizes_with_decimal_and_binary_units() {
        let cases = [
            ("0", 0),
            ("123", 123),
            ("123 B", 123),
            ("10KiB", 10 * KIB),
            ("2 GiB", 2 * GIB),
            ("1.5 GiB", GIB + GIB / 2),
            ("500 MB", 500_000_000),
            ("1 kb", 1000),
            ("1 KB", 1000),
            ("2 gib", 2 * GIB),
            ("1 TB", 1_000_000_000_000),
            ("1 TiB", 1024 * GIB),
            ("0.5 KiB", 512),
            ("0.0001 KB", 0),
            ("1.25 B", 1),
        ];
        for (text, bytes) in cases {
            assert_eq!(text.parse::<Size>().unwrap().bytes(), bytes, "{text}");
        }
    }

    #[test]
    fn refuses_what_is_not_a_size() {
        for text in [
            "",
            " ",
            "GiB",
            "-1 GiB",
            "1.5.2 GiB",
            "1. GiB",
            ".5 GiB",
            "2 GB B",
            "2 Gb",
            "2 G",
            "1 PiB",
            "2GiB ",
            " 2GiB",
            "1e3",
            "+1",
        ] {
            let err = text.parse::<Size>().unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("`{text}` is not a size like `2 GiB` or `500 MB`"),
                "{text:?}"
            );
        }
        let err = "20000000 TiB".parse::<Size>().unwrap_err();
        assert_eq!(err.to_string(), "size `20000000 TiB` is too large");
        let err = "99999999999999999999".parse::<Size>().unwrap_err();
        assert_eq!(err.to_string(), "size `99999999999999999999` is too large");
    }

    #[test]
    fn deserializes_from_text_or_a_byte_count() {
        let text: serde::de::value::StrDeserializer<'_, ValueError> = "2 KiB".into_deserializer();
        assert_eq!(Size::deserialize(text).unwrap().bytes(), 2048);
        let number: serde::de::value::U64Deserializer<ValueError> = 7_u64.into_deserializer();
        assert_eq!(Size::deserialize(number).unwrap().bytes(), 7);
        let signed: serde::de::value::I64Deserializer<ValueError> = 7_i64.into_deserializer();
        assert_eq!(Size::deserialize(signed).unwrap().bytes(), 7);
        let negative: serde::de::value::I64Deserializer<ValueError> = (-1_i64).into_deserializer();
        assert!(Size::deserialize(negative).is_err());
        let wrong: serde::de::value::BoolDeserializer<ValueError> = true.into_deserializer();
        assert!(Size::deserialize(wrong).is_err());
    }
}
