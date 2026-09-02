//! Reading a manifest out of a `BagIt` payload manifest, with the fetch file beside it.

use std::collections::BTreeMap;

use crate::digest::InteropDigest;
use crate::error::{Error, ErrorKind};
use crate::manifest::{Artifact, DigestClaims, Manifest};

use super::{malformed, unrepresentable, Context, MetadataFormat, MetadataReader};

fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_hex_32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    let raw = text.as_bytes();
    for (index, slot) in bytes.iter_mut().enumerate() {
        let high = hex_nibble(raw[index * 2])?;
        let low = hex_nibble(raw[index * 2 + 1])?;
        *slot = (high << 4) | low;
    }
    Some(bytes)
}

fn split_digest_and_path(line: &str) -> Option<(&str, &str)> {
    let boundary = line.find([' ', '\t'])?;
    let (digest, rest) = line.split_at(boundary);
    let rest = rest.trim_start_matches([' ', '\t']);
    if rest.is_empty() {
        return None;
    }
    Some((digest, rest))
}

fn algorithm_name_for_length(length: usize) -> Option<&'static str> {
    match length {
        32 => Some("MD5"),
        40 => Some("SHA-1"),
        128 => Some("SHA-512"),
        _ => None,
    }
}

struct FetchEntry {
    url: String,
    size: Option<u64>,
}

fn parse_fetch(bytes: &[u8], context: &Context<'_>) -> Result<BTreeMap<String, FetchEntry>, Error> {
    if bytes.len() as u64 > context.limits.manifest_size {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "keep fetch.txt at or under {} bytes, because this one is {} bytes",
                context.limits.manifest_size,
                bytes.len()
            ),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        malformed(MetadataFormat::BagIt, "fetch.txt is not valid UTF-8 text")
    })?;
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.splitn(3, [' ', '\t']).collect();
        let [url, length, filepath] = fields.as_slice() else {
            return Err(malformed(
                MetadataFormat::BagIt,
                "every line in fetch.txt holds a url, a length, and a filepath",
            ));
        };
        let filepath = filepath.trim_start_matches([' ', '\t']);
        if filepath.is_empty() {
            return Err(malformed(
                MetadataFormat::BagIt,
                "every line in fetch.txt holds a url, a length, and a filepath",
            ));
        }
        let size = if *length == "-" {
            None
        } else {
            Some(length.parse::<u64>().map_err(|_| {
                malformed(
                    MetadataFormat::BagIt,
                    "the length in fetch.txt is a byte count or a single dash",
                )
            })?)
        };
        entries.insert(
            filepath.to_string(),
            FetchEntry {
                url: (*url).to_string(),
                size,
            },
        );
        if entries.len() as u64 > context.limits.listing_entries {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "keep fetch.txt at or under {} entries, because it lists at least that many",
                    context.limits.listing_entries
                ),
            ));
        }
    }
    Ok(entries)
}

struct Entry {
    path: String,
    digest: InteropDigest,
}

fn parse_manifest(bytes: &[u8], context: &Context<'_>) -> Result<Vec<Entry>, Error> {
    if bytes.len() as u64 > context.limits.manifest_size {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "keep manifest-sha256.txt at or under {} bytes, because this one is {} bytes",
                context.limits.manifest_size,
                bytes.len()
            ),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        malformed(
            MetadataFormat::BagIt,
            "manifest-sha256.txt is not valid UTF-8 text",
        )
    })?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (digest_text, path) = split_digest_and_path(line).ok_or_else(|| {
            malformed(
                MetadataFormat::BagIt,
                "every line holds a digest and a filepath separated by whitespace",
            )
        })?;
        if !is_hex(digest_text) {
            return Err(malformed(
                MetadataFormat::BagIt,
                "the digest on every line is hexadecimal",
            ));
        }
        if let Some(algorithm) = algorithm_name_for_length(digest_text.len()) {
            return Err(unrepresentable(
                "this bag",
                &format!(
                    "the bag was hashed with {algorithm}, and the manifest carries BLAKE3 and SHA-256 only, which BagIt's own specification recommending SHA-512 makes common"
                ),
            ));
        }
        let bytes32 = decode_hex_32(digest_text).ok_or_else(|| {
            malformed(
                MetadataFormat::BagIt,
                "the digest on every line is sixty-four hexadecimal characters",
            )
        })?;
        entries.push(Entry {
            path: path.to_string(),
            digest: InteropDigest::from_bytes(bytes32),
        });
        if entries.len() as u64 > context.limits.listing_entries {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "keep manifest-sha256.txt at or under {} entries, because it lists at least that many",
                    context.limits.listing_entries
                ),
            ));
        }
    }
    entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(entries)
}

/// Reads a `BagIt` payload manifest, with an optional fetch file beside it, into a manifest.
pub struct BagIt;

impl BagIt {
    /// Reads the payload manifest into a manifest, taking source and size from
    /// `fetch.txt` when one is supplied.
    ///
    /// # Errors
    ///
    /// Fails with `manifest.invalid` when either document does not parse, and
    /// when the payload manifest was hashed with an algorithm the manifest
    /// does not carry.
    pub fn read_with_fetch(
        manifest_bytes: &[u8],
        fetch_bytes: Option<&[u8]>,
        context: &Context<'_>,
    ) -> Result<Manifest, Error> {
        let entries = parse_manifest(manifest_bytes, context)?;
        let fetch = fetch_bytes
            .map(|bytes| parse_fetch(bytes, context))
            .transpose()?;
        let artifacts = entries
            .into_iter()
            .map(|entry| {
                let (source, size) = match fetch.as_ref().and_then(|map| map.get(&entry.path)) {
                    Some(fetched) => (fetched.url.clone(), fetched.size),
                    None => (entry.path.clone(), None),
                };
                Artifact {
                    id: entry.path,
                    sources: vec![source],
                    size,
                    digest: Some(DigestClaims {
                        blake3: None,
                        sha256: Some(entry.digest),
                    }),
                    media_type: None,
                    archive: None,
                    select: Vec::new(),
                    layout: crate::selection::Layout::default(),
                }
            })
            .collect();
        Ok(Manifest {
            name: context.name.to_string(),
            release: None,
            artifacts,
            license: None,
        })
    }
}

impl MetadataReader for BagIt {
    fn format(&self) -> MetadataFormat {
        MetadataFormat::BagIt
    }

    fn recognizes(&self, name: &str, _bytes: &[u8]) -> bool {
        matches!(
            name,
            "manifest-sha256.txt" | "manifest-sha512.txt" | "manifest-md5.txt" | "manifest-sha1.txt"
        )
    }

    fn read(&self, bytes: &[u8], context: &Context<'_>) -> Result<Manifest, Error> {
        Self::read_with_fetch(bytes, None, context)
    }
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test assertions, where a failed read is the failure being asserted"
)]
mod tests {
    use super::*;
    use crate::limits::Limits;

    fn context<'a>(base: &'a str, name: &'a str, limits: &'a Limits) -> Context<'a> {
        Context {
            base,
            name,
            limits,
        }
    }

    #[test]
    fn reads_a_local_bag_with_no_fetch_file() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 data/z.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 data/a.bin\n";
        let manifest = BagIt.read(text, &ctx).unwrap();
        assert_eq!(manifest.name, "corpus");
        assert_eq!(manifest.artifacts.len(), 2);
        assert_eq!(manifest.artifacts[0].id, "data/a.bin");
        assert_eq!(
            manifest.artifacts[0].sources,
            vec!["data/a.bin".to_string()]
        );
        assert_eq!(manifest.artifacts[0].size, None);
        assert_eq!(manifest.artifacts[1].id, "data/z.bin");
    }

    #[test]
    fn refuses_sha512_manifests_by_name() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let sha512_of_empty = "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e";
        let text = format!("{sha512_of_empty} data/a.bin\n");
        let error = BagIt.read(text.as_bytes(), &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("SHA-512"));
        assert!(error.next_action().contains("BLAKE3 and SHA-256"));
    }

    #[test]
    fn refuses_md5_manifests_by_name() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let text = b"d41d8cd98f00b204e9800998ecf8427e data/a.bin\n";
        let error = BagIt.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("MD5"));
    }

    #[test]
    fn refuses_sha1_manifests_by_name() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let text = b"da39a3ee5e6b4b0d3255bfef95601890afd80709 data/a.bin\n";
        let error = BagIt.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("SHA-1"));
    }

    #[test]
    fn fails_on_a_malformed_line() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let text = b"no-whitespace-here\n";
        let error = BagIt.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let text = b"# a comment\n\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 data/a.bin\n";
        let manifest = BagIt.read(text, &ctx).unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
    }

    #[test]
    fn orders_entries_by_raw_path_bytes() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 data/z.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 data/a.bin\n";
        let first = BagIt.read(text, &ctx).unwrap();
        let second = BagIt.read(text, &ctx).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.artifacts[0].id, "data/a.bin");
        assert_eq!(first.artifacts[1].id, "data/z.bin");
    }

    #[test]
    fn refuses_more_entries_than_the_limit() {
        let limits = Limits {
            listing_entries: 1,
            ..Limits::default()
        };
        let ctx = context(".", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 data/a.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 data/b.bin\n";
        let error = BagIt.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ResourceLimit);
    }

    #[test]
    fn fetch_file_supplies_source_and_size_and_dash_omits_size() {
        let limits = Limits::default();
        let ctx = context(".", "corpus", &limits);
        let manifest_text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 data/a.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08 data/b.bin\n";
        let fetch_text = b"https://host/a.bin - data/a.bin\nhttps://host/b.bin 128 data/b.bin\n";
        let manifest =
            BagIt::read_with_fetch(manifest_text, Some(fetch_text), &ctx).unwrap();
        let a = manifest.artifacts.iter().find(|a| a.id == "data/a.bin").unwrap();
        assert_eq!(a.sources, vec!["https://host/a.bin".to_string()]);
        assert_eq!(a.size, None);
        let b = manifest.artifacts.iter().find(|a| a.id == "data/b.bin").unwrap();
        assert_eq!(b.sources, vec!["https://host/b.bin".to_string()]);
        assert_eq!(b.size, Some(128));
    }

    #[test]
    fn recognizes_the_manifest_names() {
        let reader = BagIt;
        assert!(reader.recognizes("manifest-sha256.txt", b""));
        assert!(reader.recognizes("manifest-sha512.txt", b""));
        assert!(reader.recognizes("manifest-md5.txt", b""));
        assert!(reader.recognizes("manifest-sha1.txt", b""));
        assert!(!reader.recognizes("bagit.txt", b""));
    }
}
