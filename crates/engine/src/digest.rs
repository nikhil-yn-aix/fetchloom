//! Digest values and the domains they belong to.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Number of bytes in every digest Fetchloom produces.
pub const DIGEST_LEN: usize = 32;

/// The derived key context separating tree digests from every other domain.
pub const TREE_DIGEST_CONTEXT: &str = "fetchloom tree digest";

/// The derived key context separating manifest digests from every other domain.
pub const MANIFEST_DIGEST_CONTEXT: &str = "fetchloom manifest digest";

/// The derived key context separating the name of a receipt from every other
/// domain.
pub const RECEIPT_KEY_CONTEXT: &str = "fetchloom receipt key";

/// The derived key context separating partial key names from every other domain.
pub const PARTIAL_KEY_CONTEXT: &str = "fetchloom partial key";

/// The derived key context separating the name a witness record is filed under
/// from every other domain.
pub const WITNESS_KEY_CONTEXT: &str = "fetchloom witness key";

/// The derived key context separating the name a resolution record is filed
/// under from every other domain.
pub const RESOLUTION_KEY_CONTEXT: &str = "fetchloom resolution key";

/// The hash algorithm a digest was produced by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Algorithm {
    /// The algorithm behind every content, tree, and manifest digest.
    Blake3,
    /// The algorithm behind every interop digest.
    Sha256,
}

impl Algorithm {
    /// Returns the prefix this algorithm is written with.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
        }
    }
}

impl fmt::Display for Algorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A digest written as an algorithm label, a colon, and lowercase hexadecimal.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest {
    algorithm: Algorithm,
    bytes: [u8; DIGEST_LEN],
}

impl Digest {
    /// Builds a digest from an algorithm and its raw bytes.
    #[must_use]
    pub fn new(algorithm: Algorithm, bytes: [u8; DIGEST_LEN]) -> Self {
        Self { algorithm, bytes }
    }

    /// Returns the algorithm that produced this digest.
    #[must_use]
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Returns the raw bytes of this digest.
    #[must_use]
    pub fn bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.bytes
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.algorithm)?;
        for byte in self.bytes {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.to_string()
    }
}

/// Why a digest string could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseDigestError {
    /// The string had no algorithm label followed by a colon.
    MissingAlgorithm,
    /// The algorithm label named no algorithm Fetchloom produces.
    UnknownAlgorithm,
    /// The hexadecimal part was not sixty-four lowercase hexadecimal characters.
    MalformedHex,
}

impl fmt::Display for ParseDigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::MissingAlgorithm => "digest has no algorithm prefix",
            Self::UnknownAlgorithm => "digest names an unknown algorithm",
            Self::MalformedHex => "digest is not sixty-four lowercase hexadecimal characters",
        };
        f.write_str(text)
    }
}

impl std::error::Error for ParseDigestError {}

impl std::str::FromStr for Digest {
    type Err = ParseDigestError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (label, hex) = text
            .split_once(':')
            .ok_or(ParseDigestError::MissingAlgorithm)?;
        let algorithm = match label {
            "blake3" => Algorithm::Blake3,
            "sha256" => Algorithm::Sha256,
            _ => return Err(ParseDigestError::UnknownAlgorithm),
        };
        if hex.len() != DIGEST_LEN * 2 {
            return Err(ParseDigestError::MalformedHex);
        }
        let hex = hex.as_bytes();
        let mut bytes = [0u8; DIGEST_LEN];
        for (index, slot) in bytes.iter_mut().enumerate() {
            let high = hex_value(hex[index * 2])?;
            let low = hex_value(hex[index * 2 + 1])?;
            *slot = (high << 4) | low;
        }
        Ok(Self { algorithm, bytes })
    }
}

impl TryFrom<String> for Digest {
    type Error = ParseDigestError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

fn hex_value(byte: u8) -> Result<u8, ParseDigestError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ParseDigestError::MalformedHex),
    }
}

/// A digest carried an algorithm its domain does not accept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongAlgorithm {
    /// The algorithm the domain requires.
    pub expected: Algorithm,
    /// The algorithm the digest carried.
    pub found: Algorithm,
}

impl fmt::Display for WrongAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected {} digest, found {}", self.expected, self.found)
    }
}

impl std::error::Error for WrongAlgorithm {}

macro_rules! domain_digest {
    ($name:ident, $algorithm:expr, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize,
        )]
        #[serde(try_from = "Digest", into = "Digest")]
        pub struct $name(Digest);

        impl $name {
            /// Builds this digest from its raw bytes.
            #[must_use]
            pub fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
                Self(Digest::new($algorithm, bytes))
            }

            /// Returns the underlying digest.
            #[must_use]
            pub fn digest(&self) -> Digest {
                self.0
            }

            /// Returns the raw bytes of this digest.
            #[must_use]
            pub fn bytes(&self) -> &[u8; DIGEST_LEN] {
                self.0.bytes()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }

        impl From<$name> for Digest {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            /// Reads a digest written as its algorithm, a colon, and lowercase
            /// hexadecimal.
            ///
            /// Fails with what the text broke when it is not a digest of this
            /// domain's algorithm.
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                let digest: Digest = text
                    .parse()
                    .map_err(|reason: ParseDigestError| reason.to_string())?;
                Self::try_from(digest).map_err(|reason| reason.to_string())
            }
        }

        impl TryFrom<Digest> for $name {
            type Error = WrongAlgorithm;

            fn try_from(digest: Digest) -> Result<Self, Self::Error> {
                if digest.algorithm() == $algorithm {
                    Ok(Self(digest))
                } else {
                    Err(WrongAlgorithm {
                        expected: $algorithm,
                        found: digest.algorithm(),
                    })
                }
            }
        }
    };
}

domain_digest!(
    ContentDigest,
    Algorithm::Blake3,
    "The root of the object bytes. The cache key and the resume authority."
);
domain_digest!(
    InteropDigest,
    Algorithm::Sha256,
    "The digest recorded for matching publisher claims. Never the cache key."
);
domain_digest!(
    TreeDigest,
    Algorithm::Blake3,
    "The digest of the canonical entry stream of a materialized directory."
);
domain_digest!(
    ManifestDigest,
    Algorithm::Blake3,
    "The digest of the canonical form of a manifest, never of its source text."
);
