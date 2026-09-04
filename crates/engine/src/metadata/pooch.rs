//! Reading a manifest out of a pooch registry file.

use crate::digest::InteropDigest;
use crate::error::{Error, ErrorKind};
use crate::manifest::{Artifact, DigestClaims, Manifest};

use super::{Context, MetadataFormat, MetadataReader, malformed, unrepresentable};

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

struct Entry {
    path: String,
    digest: InteropDigest,
}

fn parse_entries(bytes: &[u8], context: &Context<'_>) -> Result<Vec<Entry>, Error> {
    if bytes.len() as u64 > context.limits.manifest_size {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "keep the registry at or under {} bytes, because this one is {} bytes",
                context.limits.manifest_size,
                bytes.len()
            ),
        ));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| {
        malformed(
            MetadataFormat::PoochRegistry,
            "the registry is not valid UTF-8 text",
        )
    })?;
    let mut entries = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [path, hash] = fields.as_slice() else {
            return Err(malformed(
                MetadataFormat::PoochRegistry,
                "every line holds exactly a path and a hash separated by whitespace",
            ));
        };
        let digest_text = match hash.split_once(':') {
            Some((prefix, value)) => {
                if prefix != "sha256" {
                    return Err(unrepresentable(
                        "this pooch registry",
                        &format!(
                            "the hash for \"{path}\" is prefixed \"{prefix}:\", and the manifest carries BLAKE3 and SHA-256 only"
                        ),
                    ));
                }
                value
            }
            None => hash,
        };
        if !is_hex(digest_text) {
            return Err(malformed(
                MetadataFormat::PoochRegistry,
                "the hash on every line is hexadecimal",
            ));
        }
        let bytes32 = decode_hex_32(digest_text).ok_or_else(|| {
            malformed(
                MetadataFormat::PoochRegistry,
                "the hash on every line is sixty-four hexadecimal characters",
            )
        })?;
        entries.push(Entry {
            path: (*path).to_string(),
            digest: InteropDigest::from_bytes(bytes32),
        });
        if entries.len() as u64 > context.limits.listing_entries {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "keep the registry at or under {} entries, because it lists at least that many",
                    context.limits.listing_entries
                ),
            ));
        }
    }
    entries.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    Ok(entries)
}

pub struct PoochRegistry;

impl MetadataReader for PoochRegistry {
    fn format(&self) -> MetadataFormat {
        MetadataFormat::PoochRegistry
    }

    fn recognizes(&self, name: &str, _bytes: &[u8]) -> bool {
        name == "registry.txt"
            || name.ends_with(".registry")
            || name.ends_with("pooch-registry.txt")
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
        Context { base, name, limits }
    }

    #[test]
    fn reads_a_well_formed_registry() {
        let limits = Limits::default();
        let ctx = context("https://data.host/", "corpus", &limits);
        let text =
            b"data/z.csv sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
data/a.csv 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n";
        let manifest = PoochRegistry.read(text, &ctx).unwrap();
        assert_eq!(manifest.name, "corpus");
        assert_eq!(manifest.artifacts.len(), 2);
        assert_eq!(manifest.artifacts[0].id, "data/a.csv");
        assert_eq!(
            manifest.artifacts[0].sources,
            vec!["data/a.csv".to_string()]
        );
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
        assert_eq!(manifest.artifacts[1].id, "data/z.csv");
    }

    #[test]
    fn refuses_a_non_sha256_prefix_by_name() {
        let limits = Limits::default();
        let ctx = context("https://data.host/", "corpus", &limits);
        let text = b"data/a.csv md5:d41d8cd98f00b204e9800998ecf8427e\n";
        let error = PoochRegistry.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("md5:"));
        assert!(error.next_action().contains("BLAKE3 and SHA-256"));
    }

    #[test]
    fn refuses_a_sha1_prefix_by_name() {
        let limits = Limits::default();
        let ctx = context("https://data.host/", "corpus", &limits);
        let text = b"data/a.csv sha1:da39a3ee5e6b4b0d3255bfef95601890afd80709\n";
        let error = PoochRegistry.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("sha1:"));
    }

    #[test]
    fn fails_on_a_malformed_line() {
        let limits = Limits::default();
        let ctx = context("https://data.host/", "corpus", &limits);
        let text = b"data/a.csv only-one-field-should-be-two\nextra field here\n";
        let error = PoochRegistry.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let limits = Limits::default();
        let ctx = context("https://data.host/", "corpus", &limits);
        let text =
            b"# a comment\n\ndata/a.csv e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n";
        let manifest = PoochRegistry.read(text, &ctx).unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
    }

    #[test]
    fn orders_entries_by_raw_path_bytes() {
        let limits = Limits::default();
        let ctx = context("https://data.host/", "corpus", &limits);
        let text = b"z.csv e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
a.csv 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n";
        let first = PoochRegistry.read(text, &ctx).unwrap();
        let second = PoochRegistry.read(text, &ctx).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.artifacts[0].id, "a.csv");
        assert_eq!(first.artifacts[1].id, "z.csv");
    }

    #[test]
    fn refuses_more_entries_than_the_limit() {
        let limits = Limits {
            listing_entries: 1,
            ..Limits::default()
        };
        let ctx = context("https://data.host/", "corpus", &limits);
        let text = b"a.csv e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n\
b.csv 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08\n";
        let error = PoochRegistry.read(text, &ctx).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ResourceLimit);
    }

    #[test]
    fn recognizes_the_conventional_names() {
        let reader = PoochRegistry;
        assert!(reader.recognizes("registry.txt", b""));
        assert!(reader.recognizes("corpus.registry", b""));
        assert!(reader.recognizes("pooch-registry.txt", b""));
        assert!(!reader.recognizes("manifest.json", b""));
    }
}
