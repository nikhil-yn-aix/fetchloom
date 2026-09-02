//! Reading a manifest out of a `sha256sum` style checksum sidecar.

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
        _ => None,
    }
}

struct Entry {
    path: String,
    digest: InteropDigest,
}

fn parse_entries(bytes: &[u8], context: &Context<'_>) -> Result<Vec<Entry>, Error> {
    if bytes.len() as u64 > context.limits.manifest_size {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "keep the sidecar at or under {} bytes, because this one is {} bytes",
                context.limits.manifest_size,
                bytes.len()
            ),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        malformed(
            MetadataFormat::ChecksumSidecar,
            "the sidecar is not valid UTF-8 text",
        )
    })?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (digest_text, rest) = split_digest_and_path(line).ok_or_else(|| {
            malformed(
                MetadataFormat::ChecksumSidecar,
                "every line holds a digest and a path separated by whitespace",
            )
        })?;
        if !is_hex(digest_text) {
            return Err(malformed(
                MetadataFormat::ChecksumSidecar,
                "the digest on every line is hexadecimal",
            ));
        }
        if let Some(algorithm) = algorithm_name_for_length(digest_text.len()) {
            return Err(unrepresentable(
                "this checksum sidecar",
                &format!(
                    "the digest for \"{rest}\" is {} hex characters, which is {algorithm}, and the manifest carries BLAKE3 and SHA-256 only",
                    digest_text.len()
                ),
            ));
        }
        let bytes32 = decode_hex_32(digest_text).ok_or_else(|| {
            malformed(
                MetadataFormat::ChecksumSidecar,
                "the digest on every line is a length this build reads",
            )
        })?;
        let path = rest.strip_prefix('*').unwrap_or(rest);
        entries.push(Entry {
            path: path.to_string(),
            digest: InteropDigest::from_bytes(bytes32),
        });
        if entries.len() as u64 > context.limits.listing_entries {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "keep the sidecar at or under {} entries, because it lists at least that many",
                    context.limits.listing_entries
                ),
            ));
        }
    }
    entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(entries)
}

/// Reads the `sha256sum` and `shasum` sidecar convention into a manifest.
pub struct ChecksumSidecar;

impl MetadataReader for ChecksumSidecar {
    fn format(&self) -> MetadataFormat {
        MetadataFormat::ChecksumSidecar
    }

    fn recognizes(&self, name: &str, _bytes: &[u8]) -> bool {
        let lower = name.to_ascii_lowercase();
        lower.ends_with("sha256sums")
            || lower.ends_with(".sha256")
            || lower.ends_with(".sha256sum")
            || lower.ends_with("checksums.txt")
    }

    fn read(&self, bytes: &[u8], context: &Context<'_>) -> Result<Manifest, Error> {
        let entries = parse_entries(bytes, context)?;
        let artifacts = entries
            .into_iter()
            .map(|entry| Artifact {
                id: entry.path.clone(),
                sources: vec![entry.path],
                size: None,
                digest: Some(DigestClaims {
                    blake3: None,
                    sha256: Some(entry.digest),
                }),
                media_type: None,
                archive: None,
                select: Vec::new(),
                layout: crate::selection::Layout::default(),
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
    fn reads_a_well_formed_sidecar() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  b.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  a.bin\n";
        let manifest = ChecksumSidecar.read(text, &ctx).unwrap();
        assert_eq!(manifest.name, "corpus");
        assert_eq!(manifest.artifacts.len(), 2);
        assert_eq!(manifest.artifacts[0].id, "a.bin");
        assert_eq!(manifest.artifacts[0].sources, vec!["a.bin".to_string()]);
        assert_eq!(manifest.artifacts[0].size, None);
        assert_eq!(
            manifest.artifacts[0]
                .digest
                .as_ref()
                .unwrap()
                .sha256
                .unwrap()
                .to_string(),
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        assert_eq!(manifest.artifacts[1].id, "b.bin");
    }

    #[test]
    fn strips_the_binary_mode_marker() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 *b.bin\n";
        let manifest = ChecksumSidecar.read(text, &ctx).unwrap();
        assert_eq!(manifest.artifacts[0].id, "b.bin");
    }

    #[test]
    fn refuses_md5_by_name() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"d41d8cd98f00b204e9800998ecf8427e  empty.bin\n";
        let error = ChecksumSidecar.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("MD5"));
        assert!(error.next_action().contains("BLAKE3 and SHA-256"));
    }

    #[test]
    fn refuses_sha1_by_name() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"da39a3ee5e6b4b0d3255bfef95601890afd80709  empty.bin\n";
        let error = ChecksumSidecar.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("SHA-1"));
        assert!(error.next_action().contains("BLAKE3 and SHA-256"));
    }

    #[test]
    fn fails_on_a_malformed_line() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"not-a-digest-or-a-path-with-whitespace\n";
        let error = ChecksumSidecar.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"# a comment\n\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  a.bin\n";
        let manifest = ChecksumSidecar.read(text, &ctx).unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
    }

    #[test]
    fn orders_entries_by_raw_path_bytes() {
        let limits = Limits::default();
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  z.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  a.bin\n";
        let first = ChecksumSidecar.read(text, &ctx).unwrap();
        let second = ChecksumSidecar.read(text, &ctx).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.artifacts[0].id, "a.bin");
        assert_eq!(first.artifacts[1].id, "z.bin");
    }

    #[test]
    fn refuses_more_entries_than_the_limit() {
        let limits = Limits {
            listing_entries: 1,
            ..Limits::default()
        };
        let ctx = context("https://host/data/", "corpus", &limits);
        let text = b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  a.bin\n\
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  b.bin\n";
        let error = ChecksumSidecar.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ResourceLimit);
    }

    #[test]
    fn recognizes_the_conventional_names() {
        let reader = ChecksumSidecar;
        assert!(reader.recognizes("SHA256SUMS", b""));
        assert!(reader.recognizes("sha256sums", b""));
        assert!(reader.recognizes("corpus.sha256", b""));
        assert!(reader.recognizes("corpus.sha256sum", b""));
        assert!(reader.recognizes("checksums.txt", b""));
        assert!(!reader.recognizes("manifest.json", b""));
    }
}
