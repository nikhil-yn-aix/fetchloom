//! SHA-256, a file's identity in the cache and in `data.lock`, and MD5, which some sources state
//! and which is checked, never trusted as an identity.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use md5::Digest as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// the SHA-256 of a file's bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Sha256([u8; 32]);

/// the MD5 of a file's bytes, as zenodo, figshare and dataverse state it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Md5([u8; 16]);

impl Sha256 {
    /// reads 64 hex digits, in either case.
    pub(crate) fn from_hex(text: &str) -> Option<Sha256> {
        from_hex(text).map(Sha256)
    }

    pub(crate) fn hex(&self) -> String {
        hex(&self.0)
    }

    /// the first 12 hex digits, enough to compare two hashes at a glance.
    pub(crate) fn short(&self) -> String {
        let mut text = self.hex();
        text.truncate(12);
        text
    }

    /// the digest as raw bytes, for hashes built from other hashes.
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// hashes a whole file, reading it once.
    pub(crate) fn of_file(path: &Path) -> io::Result<Sha256> {
        let mut file = File::open(path)?;
        let mut hasher = Hasher::default();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                return Ok(hasher.finish().0);
            }
            hasher.update(&buffer[..read]);
        }
    }
}

impl Md5 {
    /// reads 32 hex digits, in either case.
    pub(crate) fn from_hex(text: &str) -> Option<Md5> {
        from_hex(text).map(Md5)
    }

    pub(crate) fn hex(&self) -> String {
        hex(&self.0)
    }
}

fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (index, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let high = char::from(*high).to_digit(16)?;
        let low = char::from(*low).to_digit(16)?;
        out[index] = u8::try_from(high * 16 + low).ok()?;
    }
    Some(out)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

macro_rules! text_forms {
    ($type:ident, $name:literal, $digits:literal) => {
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.hex())
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<$type, D::Error> {
                let text = String::deserialize(deserializer)?;
                $type::from_hex(&text).ok_or_else(|| {
                    D::Error::custom(concat!("an ", $name, " is ", $digits, " hex digits"))
                })
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.hex())
            }
        }

        impl fmt::Debug for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($name, ":{}"), self.hex())
            }
        }
    };
}

text_forms!(Sha256, "SHA-256", "64");
text_forms!(Md5, "MD5", "32");

/// hashes computed while bytes stream past: always SHA-256, and MD5 when one will be checked.
#[derive(Clone, Default)]
pub(crate) struct Hasher {
    sha256: sha2::Sha256,
    md5: Option<md5::Md5>,
}

impl Hasher {
    pub(crate) fn with_md5() -> Hasher {
        Hasher {
            md5: Some(md5::Md5::default()),
            ..Hasher::default()
        }
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        sha2::Digest::update(&mut self.sha256, bytes);
        if let Some(md5) = &mut self.md5 {
            md5.update(bytes);
        }
    }

    pub(crate) fn finish(self) -> (Sha256, Option<Md5>) {
        let sha256 = Sha256(sha2::Digest::finalize(self.sha256).into());
        (sha256, self.md5.map(|md5| Md5(md5.finalize().into())))
    }

    /// the SHA-256 of the bytes so far. the hasher goes on taking more.
    pub(crate) fn sha256_so_far(&self) -> Sha256 {
        Sha256(sha2::Digest::finalize(self.sha256.clone()).into())
    }
}

/// the SHA-256 of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> Sha256 {
    let mut hasher = Hasher::default();
    hasher.update(bytes);
    hasher.finish().0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_the_published_test_vectors() {
        let mut hasher = Hasher::with_md5();
        hasher.update(b"abc");
        let (sha256, md5) = hasher.finish();
        assert_eq!(
            sha256.hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            md5.map(|md5| md5.hex()).as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(
            Hasher::default().finish().1,
            None,
            "md5 costs nothing unless asked for"
        );
    }

    #[test]
    fn hex_round_trips_and_ignores_case() {
        let digest = sha256(b"fetchloom");
        assert_eq!(Sha256::from_hex(&digest.hex()), Some(digest));
        assert_eq!(Sha256::from_hex(&digest.hex().to_uppercase()), Some(digest));
        let md5 = Md5::from_hex("900150983CD24FB0D6963F7D28E17F72").unwrap();
        assert_eq!(md5.hex(), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn rejects_anything_but_the_right_number_of_hex_digits() {
        assert_eq!(Sha256::from_hex("abc"), None);
        assert_eq!(Sha256::from_hex(&"g".repeat(64)), None);
        assert_eq!(Md5::from_hex(&"a".repeat(64)), None);
        assert_eq!(Sha256::from_hex(&"é".repeat(32)), None);
    }
}
