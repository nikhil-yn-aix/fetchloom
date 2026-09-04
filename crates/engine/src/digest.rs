//! Digest values and the domains they belong to.

use std::fmt;

use serde::{Deserialize, Serialize};

pub const DIGEST_LEN: usize = 32;

pub const TREE_DIGEST_CONTEXT: &str = "fetchloom tree digest";

pub const MANIFEST_DIGEST_CONTEXT: &str = "fetchloom manifest digest";

pub const RECEIPT_KEY_CONTEXT: &str = "fetchloom receipt key";

pub const PARTIAL_KEY_CONTEXT: &str = "fetchloom partial key";

pub const WITNESS_KEY_CONTEXT: &str = "fetchloom witness key";

pub const RESOLUTION_KEY_CONTEXT: &str = "fetchloom resolution key";

pub const MEASUREMENT_KEY_CONTEXT: &str = "fetchloom measurement key";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Algorithm {
    Blake3,
    Sha256,
}

impl Algorithm {
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

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest {
    algorithm: Algorithm,
    bytes: [u8; DIGEST_LEN],
}

impl Digest {
    #[must_use]
    pub fn new(algorithm: Algorithm, bytes: [u8; DIGEST_LEN]) -> Self {
        Self { algorithm, bytes }
    }

    #[must_use]
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseDigestError {
    MissingAlgorithm,
    UnknownAlgorithm,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrongAlgorithm {
    pub expected: Algorithm,
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
            #[must_use]
            pub fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
                Self(Digest::new($algorithm, bytes))
            }

            #[must_use]
            pub fn digest(&self) -> Digest {
                self.0
            }

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
