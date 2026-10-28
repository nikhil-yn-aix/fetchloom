//! SHA-256, a file's identity in the cache and in `data.lock`.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Digest as _;

/// the SHA-256 of a file's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Sha256([u8; 32]);

impl Sha256 {
    /// reads 64 hex digits, in either case.
    pub(crate) fn from_hex(text: &str) -> Option<Sha256> {
        let bytes = text.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (index, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
            let high = char::from(*high).to_digit(16)?;
            let low = char::from(*low).to_digit(16)?;
            out[index] = u8::try_from(high * 16 + low).ok()?;
        }
        Some(Sha256(out))
    }

    pub(crate) fn hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut text = String::with_capacity(64);
        for byte in self.0 {
            text.push(char::from(DIGITS[usize::from(byte >> 4)]));
            text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
        }
        text
    }

    /// hashes a whole file, reading it once.
    pub(crate) fn of_file(path: &Path) -> io::Result<Sha256> {
        let mut file = File::open(path)?;
        let mut hasher = Hasher::default();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                return Ok(hasher.finish());
            }
            hasher.update(&buffer[..read]);
        }
    }
}

impl Serialize for Sha256 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Sha256 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Sha256, D::Error> {
        let text = String::deserialize(deserializer)?;
        Sha256::from_hex(&text).ok_or_else(|| D::Error::custom("a SHA-256 is 64 hex digits"))
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

/// a SHA-256 computed while bytes stream past.
#[derive(Default)]
pub(crate) struct Hasher(sha2::Sha256);

impl Hasher {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub(crate) fn finish(self) -> Sha256 {
        Sha256(self.0.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(bytes: &[u8]) -> Sha256 {
        let mut hasher = Hasher::default();
        hasher.update(bytes);
        hasher.finish()
    }

    #[test]
    fn matches_the_published_test_vector() {
        assert_eq!(
            of(b"abc").hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hex_round_trips_and_ignores_case() {
        let digest = of(b"fetchloom");
        assert_eq!(Sha256::from_hex(&digest.hex()), Some(digest));
        assert_eq!(Sha256::from_hex(&digest.hex().to_uppercase()), Some(digest));
    }

    #[test]
    fn rejects_anything_but_64_hex_digits() {
        assert_eq!(Sha256::from_hex("abc"), None);
        assert_eq!(Sha256::from_hex(&"g".repeat(64)), None);
    }
}
