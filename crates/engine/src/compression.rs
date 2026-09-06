//! What the run was asked to do with a cached object's bytes on the way to disk.

use std::fmt;

pub const MIN_LEVEL: i32 = 1;

pub const MAX_LEVEL: i32 = 19;

pub const PROBE_HEAD_BYTES: usize = 1 << 20;

pub const PROBE_RATIO: f64 = 1.10;

/// The strides the probe measures. One is the unshuffled measurement, because
/// rearranging one byte words is a copy, so a stride of one never reaches disk
/// and is spelled zero there.
pub const PROBE_STRIDES: [u8; 4] = [1, 2, 4, 8];

pub const MAX_SHUFFLE_STRIDE: u8 = 8;

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
pub enum Stored {
    Raw,
    Zstd {
        level: i32,
        stride: u8,
        dictionary: u32,
    },
}

impl Stored {
    #[must_use]
    pub fn describe(self) -> String {
        match self {
            Self::Raw => "stored raw".to_owned(),
            Self::Zstd {
                level,
                stride,
                dictionary,
            } => format!(
                "stored zstd:{level}, {}, {}",
                stride_label(stride),
                dictionary_label(dictionary)
            ),
        }
    }
}

#[must_use]
pub fn stride_label(stride: u8) -> String {
    if stride < 2 {
        "no byte shuffle".to_owned()
    } else {
        format!("byte-shuffled at stride {stride}")
    }
}

#[must_use]
pub fn dictionary_label(dictionary: u32) -> String {
    if dictionary == 0 {
        "against no dictionary".to_owned()
    } else {
        format!("against dictionary {dictionary}")
    }
}

/// Rearranges bytes so that the byte at each position of a stride-sized word
/// sits beside the same position of every other word. The exponent bytes of a
/// float array repeat and its mantissa bytes do not, so grouping like
/// positions is what makes dense numeric data compressible at all.
///
/// A stride below two is a copy, because one byte words are already grouped.
#[must_use]
pub fn shuffle(bytes: &[u8], stride: u8) -> Vec<u8> {
    let stride = usize::from(stride);
    if stride < 2 {
        return bytes.to_vec();
    }
    let whole = bytes.len() / stride * stride;
    let mut out = Vec::with_capacity(bytes.len());
    for position in 0..stride {
        let mut at = position;
        while at < whole {
            out.push(bytes[at]);
            at += stride;
        }
    }
    out.extend_from_slice(&bytes[whole..]);
    out
}

/// The inverse of [`shuffle`], at the same stride.
#[must_use]
pub fn unshuffle(bytes: &[u8], stride: u8) -> Vec<u8> {
    let stride = usize::from(stride);
    if stride < 2 {
        return bytes.to_vec();
    }
    let whole = bytes.len() / stride * stride;
    let words = whole / stride;
    let mut out = vec![0u8; bytes.len()];
    let mut taken = 0;
    for position in 0..stride {
        for word in 0..words {
            out[word * stride + position] = bytes[taken];
            taken += 1;
        }
    }
    out[whole..].copy_from_slice(&bytes[whole..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{CompressionChoice, PROBE_STRIDES, shuffle, unshuffle};

    #[test]
    fn shuffling_is_reversible_at_every_length_around_every_stride() {
        for stride in PROBE_STRIDES {
            for length in 0..40 {
                let bytes: Vec<u8> = (0..length)
                    .map(|index: u8| index.wrapping_mul(7).wrapping_add(3))
                    .collect();
                assert_eq!(
                    unshuffle(&shuffle(&bytes, stride), stride),
                    bytes,
                    "a {length} byte buffer did not survive stride {stride}"
                );
            }
        }
    }

    #[test]
    fn shuffling_groups_the_like_positions_of_each_word() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(shuffle(&bytes, 4), vec![1, 5, 2, 6, 3, 7, 4, 8]);
        assert_eq!(shuffle(&bytes, 2), vec![1, 3, 5, 7, 2, 4, 6, 8]);
        assert_eq!(shuffle(&bytes, 8), bytes.to_vec());
    }

    #[test]
    fn a_stride_of_one_is_a_copy_because_one_byte_words_are_already_grouped() {
        let bytes = [9, 8, 7, 6, 5];
        assert_eq!(shuffle(&bytes, 1), bytes.to_vec());
        assert_eq!(shuffle(&bytes, 0), bytes.to_vec());
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
