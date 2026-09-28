//! Content digests: an algorithm and its bytes, written as `blake3:<hex>`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{hex, text};

/// A hash algorithm fetchloom computes or checks.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum Algorithm {
    /// BLAKE3, the identity of every object.
    Blake3,
    /// SHA-256, as stated by publishers.
    Sha256,
    /// SHA-1, as stated by publishers.
    Sha1,
    /// MD5, as stated by publishers.
    Md5,
}

impl Algorithm {
    /// The lowercase name used in digests and lock files, such as `blake3`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
            Self::Sha1 => "sha1",
            Self::Md5 => "md5",
        }
    }

    const fn hex_len(self) -> usize {
        match self {
            Self::Blake3 | Self::Sha256 => 64,
            Self::Sha1 => 40,
            Self::Md5 => 32,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [Self::Blake3, Self::Sha256, Self::Sha1, Self::Md5]
            .into_iter()
            .find(|algorithm| algorithm.name() == name)
    }
}

/// A digest: the algorithm that produced it and its bytes.
///
/// Digests of different algorithms are never equal, even when their bytes are. The text form is
/// the algorithm name, a colon and lowercase hex, such as `blake3:1c9e04a7...`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Digest {
    /// A BLAKE3 digest.
    Blake3([u8; 32]),
    /// A SHA-256 digest.
    Sha256([u8; 32]),
    /// A SHA-1 digest.
    Sha1([u8; 20]),
    /// An MD5 digest.
    Md5([u8; 16]),
}

impl Digest {
    /// Reads a digest of `algorithm` from bare lowercase hex, as lock files store file hashes.
    ///
    /// # Errors
    ///
    /// Returns [`DigestError::Hex`] when `hex` is not lowercase hex of the algorithm's length.
    pub fn from_hex(algorithm: Algorithm, hex: &str) -> Result<Self, DigestError> {
        let digest = match algorithm {
            Algorithm::Blake3 => hex::decode(hex).map(Self::Blake3),
            Algorithm::Sha256 => hex::decode(hex).map(Self::Sha256),
            Algorithm::Sha1 => hex::decode(hex).map(Self::Sha1),
            Algorithm::Md5 => hex::decode(hex).map(Self::Md5),
        };
        digest.ok_or_else(|| DigestError::Hex {
            algorithm,
            length: algorithm.hex_len(),
            text: hex.to_owned(),
        })
    }

    /// The algorithm that produced this digest.
    #[must_use]
    pub const fn algorithm(&self) -> Algorithm {
        match self {
            Self::Blake3(_) => Algorithm::Blake3,
            Self::Sha256(_) => Algorithm::Sha256,
            Self::Sha1(_) => Algorithm::Sha1,
            Self::Md5(_) => Algorithm::Md5,
        }
    }

    /// The raw digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Blake3(bytes) | Self::Sha256(bytes) => bytes,
            Self::Sha1(bytes) => bytes,
            Self::Md5(bytes) => bytes,
        }
    }

    /// The bytes as lowercase hex, without the algorithm name.
    pub(crate) fn write_hex(&self, out: &mut impl fmt::Write) -> fmt::Result {
        hex::write(self.as_bytes(), out)
    }

    /// The short form for people: the algorithm name and the first 12 hex characters.
    #[must_use]
    pub fn short(&self) -> impl fmt::Display + '_ {
        Short(self)
    }
}

struct Short<'a>(&'a Digest);

impl fmt::Display for Short<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.0.algorithm().name())?;
        hex::write(&self.0.as_bytes()[..6], f)
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.algorithm().name())?;
        self.write_hex(f)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (name, hex) = text
            .split_once(':')
            .ok_or_else(|| DigestError::NoAlgorithm(text.to_owned()))?;
        let algorithm =
            Algorithm::from_name(name).ok_or_else(|| DigestError::Algorithm(name.to_owned()))?;
        Self::from_hex(algorithm, hex)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        text::deserialize(deserializer)
    }
}

/// Why text is not a digest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DigestError {
    /// The text has no `algorithm:` prefix.
    #[error("digest `{0}` has no algorithm, expected `blake3:<hex>`")]
    NoAlgorithm(String),
    /// The algorithm name is not one fetchloom knows.
    #[error("unknown digest algorithm `{0}`, expected blake3, sha256, sha1 or md5")]
    Algorithm(String),
    /// The hex part has the wrong length or a character that is not lowercase hex.
    #[error("{} digests are {length} lowercase hex characters, got `{text}`", algorithm.name())]
    Hex {
        /// The algorithm the hex was read for.
        algorithm: Algorithm,
        /// The number of hex characters the algorithm needs.
        length: usize,
        /// The text that was refused.
        text: String,
    },
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde::de::IntoDeserializer;
    use serde::de::value::{Error as ValueError, StrDeserializer};

    use super::*;

    const B3: &str = "blake3:1c9e04a7b2f1aa000000000000000000000000000000000000000000000000ff";

    #[test]
    fn parses_and_displays_every_algorithm() {
        let cases = [
            (Algorithm::Blake3, "blake3", 32),
            (Algorithm::Sha256, "sha256", 32),
            (Algorithm::Sha1, "sha1", 20),
            (Algorithm::Md5, "md5", 16),
        ];
        for (algorithm, name, len) in cases {
            let text = format!("{name}:{}", "0a".repeat(len));
            let digest: Digest = text.parse().unwrap();
            assert_eq!(digest.algorithm(), algorithm);
            assert_eq!(digest.algorithm().name(), name);
            assert_eq!(digest.as_bytes(), vec![0x0a; len].as_slice());
            assert_eq!(digest.to_string(), text);
            assert_eq!(format!("{digest:?}"), text);
        }
    }

    #[test]
    fn short_form_is_the_algorithm_and_twelve_hex_characters() {
        let digest: Digest = B3.parse().unwrap();
        assert_eq!(digest.short().to_string(), "blake3:1c9e04a7b2f1");
    }

    #[test]
    fn equal_bytes_of_two_algorithms_are_not_equal() {
        let bytes = [7; 32];
        assert_ne!(Digest::Blake3(bytes), Digest::Sha256(bytes));
        assert_eq!(Digest::Blake3(bytes), Digest::Blake3(bytes));
    }

    #[test]
    fn from_hex_reads_a_bare_hex_string_for_one_algorithm() {
        let digest = Digest::from_hex(Algorithm::Md5, &"ab".repeat(16)).unwrap();
        assert_eq!(digest, Digest::Md5([0xab; 16]));
        let err = Digest::from_hex(Algorithm::Md5, "ab").unwrap_err();
        assert_eq!(
            err.to_string(),
            "md5 digests are 32 lowercase hex characters, got `ab`"
        );
    }

    #[test]
    fn refuses_text_that_is_not_a_digest() {
        let long = "a".repeat(64);
        let cases = [
            (
                long.clone(),
                format!("digest `{long}` has no algorithm, expected `blake3:<hex>`"),
            ),
            (
                format!("sha512:{long}"),
                "unknown digest algorithm `sha512`, expected blake3, sha256, sha1 or md5"
                    .to_owned(),
            ),
            (
                format!("blake3:{}", "A".repeat(64)),
                format!(
                    "blake3 digests are 64 lowercase hex characters, got `{}`",
                    "A".repeat(64)
                ),
            ),
            (
                format!("blake3:{}", "a".repeat(63)),
                format!(
                    "blake3 digests are 64 lowercase hex characters, got `{}`",
                    "a".repeat(63)
                ),
            ),
            (
                format!("blake3:{}g", "a".repeat(63)),
                format!(
                    "blake3 digests are 64 lowercase hex characters, got `{}g`",
                    "a".repeat(63)
                ),
            ),
            (
                "BLAKE3:00".to_owned(),
                "unknown digest algorithm `BLAKE3`, expected blake3, sha256, sha1 or md5"
                    .to_owned(),
            ),
        ];
        for (text, message) in cases {
            let err = text.parse::<Digest>().unwrap_err();
            assert_eq!(err.to_string(), message, "{text}");
        }
    }

    #[test]
    fn deserializes_from_a_string() {
        let de: StrDeserializer<'_, ValueError> = B3.into_deserializer();
        assert_eq!(Digest::deserialize(de).unwrap(), B3.parse().unwrap());
        let de: StrDeserializer<'_, ValueError> = "blake3:00".into_deserializer();
        assert!(Digest::deserialize(de).is_err());
    }
}
