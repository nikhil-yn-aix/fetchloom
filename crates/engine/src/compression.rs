//! What the run was asked to do with a cached object's bytes on the way to disk.

use std::fmt;

pub const MIN_LEVEL: i32 = 1;

pub const MAX_LEVEL: i32 = 19;

pub const PROBE_HEAD_BYTES: usize = 1 << 20;

pub const PROBE_RATIO: f64 = 1.10;

pub const SHUFFLE_STRIDE: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionChoice {
    Auto,
    None,
    Zstd(i32),
}

impl CompressionChoice {
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::None => "none".to_owned(),
            Self::Zstd(level) => format!("zstd:{level}"),
        }
    }
}

impl fmt::Display for CompressionChoice {
    fn fmt(&self, into: &mut fmt::Formatter<'_>) -> fmt::Result {
        into.write_str(&self.label())
    }
}

impl std::str::FromStr for CompressionChoice {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refusal =
            || format!("{text} is not auto, none, or zstd:n where n is {MIN_LEVEL} to {MAX_LEVEL}");
        match text {
            "auto" => Ok(Self::Auto),
            "none" => Ok(Self::None),
            _ => {
                let level = text.strip_prefix("zstd:").ok_or_else(refusal)?;
                let level: i32 = level.parse().map_err(|_| refusal())?;
                if (MIN_LEVEL..=MAX_LEVEL).contains(&level) {
                    Ok(Self::Zstd(level))
                } else {
                    Err(refusal())
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    None,
    Shuffle,
}

impl Transform {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "no byte transform",
            Self::Shuffle => "byte-shuffled at stride 4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stored {
    Raw,
    Zstd { level: i32, transform: Transform },
}

impl Stored {
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Raw => "stored raw".to_owned(),
            Self::Zstd { level, transform } => {
                format!("stored zstd:{level}, {}", transform.label())
            }
        }
    }
}

/// Rearranges bytes so that the byte at each position of a stride-sized word
/// sits beside the same position of every other word. The exponent bytes of a
/// float array repeat and its mantissa bytes do not, so grouping like
/// positions is what makes dense numeric data compressible at all.
#[must_use]
pub fn shuffle(bytes: &[u8]) -> Vec<u8> {
    let whole = bytes.len() / SHUFFLE_STRIDE * SHUFFLE_STRIDE;
    let mut out = Vec::with_capacity(bytes.len());
    for position in 0..SHUFFLE_STRIDE {
        let mut at = position;
        while at < whole {
            out.push(bytes[at]);
            at += SHUFFLE_STRIDE;
        }
    }
    out.extend_from_slice(&bytes[whole..]);
    out
}

/// The inverse of [`shuffle`].
#[must_use]
pub fn unshuffle(bytes: &[u8]) -> Vec<u8> {
    let whole = bytes.len() / SHUFFLE_STRIDE * SHUFFLE_STRIDE;
    let words = whole / SHUFFLE_STRIDE;
    let mut out = vec![0u8; bytes.len()];
    let mut taken = 0;
    for position in 0..SHUFFLE_STRIDE {
        for word in 0..words {
            out[word * SHUFFLE_STRIDE + position] = bytes[taken];
            taken += 1;
        }
    }
    out[whole..].copy_from_slice(&bytes[whole..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{CompressionChoice, shuffle, unshuffle};

    #[test]
    fn shuffling_is_reversible_at_every_length_around_the_stride() {
        for length in 0..40 {
            let bytes: Vec<u8> = (0..length)
                .map(|index: u8| index.wrapping_mul(7).wrapping_add(3))
                .collect();
            assert_eq!(
                unshuffle(&shuffle(&bytes)),
                bytes,
                "a {length} byte buffer did not survive the round trip"
            );
        }
    }

    #[test]
    fn shuffling_groups_the_like_positions_of_each_word() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(shuffle(&bytes), vec![1, 5, 2, 6, 3, 7, 4, 8]);
    }

    #[test]
    fn a_level_outside_the_range_is_refused() {
        assert!("zstd:0".parse::<CompressionChoice>().is_err());
        assert!("zstd:20".parse::<CompressionChoice>().is_err());
        assert_eq!(
            "zstd:19".parse::<CompressionChoice>(),
            Ok(CompressionChoice::Zstd(19))
        );
    }
}
