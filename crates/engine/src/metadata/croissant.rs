//! Reading a manifest out of a Croissant dataset description.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::digest::InteropDigest;
use crate::document::{self, Syntax};
use crate::error::{Error, ErrorKind};
use crate::manifest::{Artifact, DigestClaims, Manifest};
use crate::metadata::{Context, MetadataFormat, MetadataReader, malformed, unrepresentable};
use crate::selection::{Glob, Layout};

#[derive(Clone, Copy, Debug, Default)]
pub struct Croissant;

impl MetadataReader for Croissant {
    fn format(&self) -> MetadataFormat {
        MetadataFormat::Croissant
    }

    fn recognizes(&self, _name: &str, bytes: &[u8]) -> bool {
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(bytes) else {
            return false;
        };
        let context_names_croissant = fields
            .get("@context")
            .is_some_and(|value| value.to_string().to_lowercase().contains("croissant"));
        if context_names_croissant {
            return true;
        }
        let type_names_dataset = fields
            .get("@type")
            .is_some_and(|value| type_matches(value, "Dataset"));
        type_names_dataset && fields.get("distribution").is_some_and(Value::is_array)
    }

    fn read(&self, bytes: &[u8], context: &Context<'_>) -> Result<Manifest, Error> {
        let document = document::parse(bytes, Syntax::Json, context.limits)?;
        let Value::Object(root) = &document else {
            return Err(malformed(self.format(), "the top level is a JSON object"));
        };
        let name = root
            .get("name")
            .and_then(Value::as_str)
            .map_or_else(|| context.name.to_owned(), str::to_owned);
        let release = root
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let distribution = root
            .get("distribution")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                malformed(
                    self.format(),
                    "name a distribution array of file objects and file sets",
                )
            })?;

        let mut artifacts: Vec<Artifact> = Vec::new();
        let mut object_index: HashMap<String, usize> = HashMap::new();
        let mut file_sets: Vec<&Map<String, Value>> = Vec::new();

        for node in distribution {
            let Value::Object(fields) = node else {
                return Err(malformed(
                    self.format(),
                    "each distribution entry is a JSON object",
                ));
            };
            let node_type = fields.get("@type");
            let is_file_object = node_type.is_some_and(|value| type_matches(value, "FileObject"));
            let is_file_set = node_type.is_some_and(|value| type_matches(value, "FileSet"));
            if is_file_object {
                let artifact = read_file_object(fields)?;
                object_index.insert(artifact.id.clone(), artifacts.len());
                artifacts.push(artifact);
            } else if is_file_set {
                file_sets.push(fields);
            }
        }

        for fields in file_sets {
            let contained = fields
                .get("containedIn")
                .map(contained_in_ids)
                .unwrap_or_default();
            if contained.len() != 1 {
                return Err(unrepresentable(
                    "a Croissant FileSet spanning several containers",
                    "the FileSet names more than one containedIn target, which draws one \
                     logical collection from several archives and the manifest's selection \
                     belongs to one artifact",
                ));
            }
            let Some(&index) = object_index.get(&contained[0]) else {
                return Err(unrepresentable(
                    "a Croissant FileSet whose containedIn names another FileSet",
                    "the containedIn target is not one FileObject in this distribution, which \
                     draws one logical collection from several archives and the manifest's \
                     selection belongs to one artifact",
                ));
            };
            if fields.get("excludes").is_some() {
                return Err(unrepresentable(
                    "a Croissant FileSet excludes list",
                    "the manifest states includes only, and the exclusion cannot be represented \
                     without changing what is selected",
                ));
            }
            artifacts[index].select = read_globs(fields.get("includes"))?;
        }

        if artifacts.len() as u64 > context.limits.listing_entries {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "reduce the distribution to at most {} file objects, because this document \
                     names {}",
                    context.limits.listing_entries,
                    artifacts.len()
                ),
            ));
        }
        if artifacts.is_empty() {
            return Err(malformed(
                self.format(),
                "name at least one file object with a location",
            ));
        }

        Ok(Manifest {
            name,
            release,
            artifacts,
            license: None,
        })
    }
}

fn type_matches(value: &Value, want: &str) -> bool {
    match value {
        Value::String(text) => text.rsplit(':').next() == Some(want),
        Value::Array(items) => items.iter().any(|item| type_matches(item, want)),
        _ => false,
    }
}

fn contained_in_ids(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Object(fields) => fields
            .get("@id")
            .and_then(Value::as_str)
            .map(|id| vec![id.to_owned()])
            .unwrap_or_default(),
        Value::Array(items) => items.iter().flat_map(contained_in_ids).collect(),
        _ => Vec::new(),
    }
}

fn read_file_object(fields: &Map<String, Value>) -> Result<Artifact, Error> {
    let id = fields
        .get("@id")
        .and_then(Value::as_str)
        .or_else(|| fields.get("name").and_then(Value::as_str))
        .ok_or_else(|| {
            malformed(
                MetadataFormat::Croissant,
                "give each FileObject an @id or a name",
            )
        })?
        .to_owned();

    if fields.get("md5").is_some() {
        return Err(unrepresentable(
            "a Croissant md5 digest",
            "the FileObject states an md5 checksum, which this build does not carry a slot for",
        ));
    }

    let content_url = fields.get("contentUrl").and_then(Value::as_str);
    let Some(url) = content_url else {
        return Err(unrepresentable(
            "a Croissant FileObject naming no contentUrl",
            if fields.get("containedIn").is_some() {
                "the FileObject states only a containedIn, so it is one member of another object rather than an object, and a manifest artifact names bytes that can be fetched"
            } else {
                "the FileObject states no contentUrl and no containedIn, so it names no location"
            },
        ));
    };
    let sources = vec![url.to_owned()];

    let size = read_content_size(fields.get("contentSize"))?;

    let digest = match fields.get("sha256").and_then(Value::as_str) {
        Some(hex) => {
            let interop: InteropDigest = format!("sha256:{hex}").parse().map_err(|_| {
                malformed(
                    MetadataFormat::Croissant,
                    "sha256 is sixty-four lowercase hexadecimal characters",
                )
            })?;
            Some(DigestClaims {
                blake3: None,
                sha256: Some(interop),
            })
        }
        None => None,
    };

    let media_type = fields
        .get("encodingFormat")
        .and_then(Value::as_str)
        .map(str::to_owned);

    Ok(Artifact {
        id,
        sources,
        size,
        digest,
        media_type,
        archive: None,
        select: Vec::new(),
        layout: Layout::default(),
    })
}

fn read_content_size(value: Option<&Value>) -> Result<Option<u64>, Error> {
    match value {
        None => Ok(None),
        Some(Value::Number(number)) => {
            let bytes = number.as_u64().ok_or_else(|| {
                unrepresentable(
                    "a Croissant contentSize",
                    "the value is not a whole number of bytes",
                )
            })?;
            Ok(Some(bytes))
        }
        Some(Value::String(text)) => {
            let bytes: u64 = text.parse().map_err(|_| {
                unrepresentable(
                    "a Croissant contentSize",
                    &format!(
                        "\"{text}\" states a size as a rendered quantity rather than a byte count"
                    ),
                )
            })?;
            Ok(Some(bytes))
        }
        Some(_) => Err(malformed(
            MetadataFormat::Croissant,
            "contentSize is a number or a string",
        )),
    }
}

fn read_globs(value: Option<&Value>) -> Result<Vec<Glob>, Error> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(pattern)) => Ok(vec![Glob::new(pattern.clone())]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(|text| Glob::new(text.to_owned()))
                    .ok_or_else(|| {
                        malformed(
                            MetadataFormat::Croissant,
                            "includes is a glob string or an array of glob strings",
                        )
                    })
            })
            .collect(),
        Some(_) => Err(malformed(
            MetadataFormat::Croissant,
            "includes is a glob string or an array of glob strings",
        )),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "test setup, where a failure to build the input is the assertion"
    )]

    use super::Croissant;
    use crate::error::ErrorKind;
    use crate::limits::Limits;
    use crate::metadata::{Context, MetadataReader};
    use crate::selection::Layout;

    fn context() -> (Limits, String, String) {
        (
            Limits::default(),
            "https://example.org/".to_owned(),
            "fallback".to_owned(),
        )
    }

    fn read(
        reader: Croissant,
        bytes: &[u8],
    ) -> Result<crate::manifest::Manifest, crate::error::Error> {
        let (limits, base, name) = context();
        reader.read(
            bytes,
            &Context {
                base: &base,
                name: &name,
                limits: &limits,
            },
        )
    }

    #[test]
    fn a_well_formed_document_reads_into_the_expected_manifest() {
        let document = br#"{
            "@context": {"@vocab": "https://schema.org/", "cr": "http://mlcommons.org/croissant/"},
            "@type": "cr:Dataset",
            "name": "movielens",
            "version": "25m",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "ml-25m.zip",
                    "contentUrl": "https://files.grouplens.org/ml-25m.zip",
                    "contentSize": 261978986,
                    "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "encodingFormat": "application/zip"
                }
            ]
        }"#;
        let reader = Croissant;
        let manifest = read(reader, document).unwrap();
        assert_eq!(manifest.name, "movielens");
        assert_eq!(manifest.release.as_deref(), Some("25m"));
        assert_eq!(manifest.artifacts.len(), 1);
        let artifact = &manifest.artifacts[0];
        assert_eq!(artifact.id, "ml-25m.zip");
        assert_eq!(
            artifact.sources,
            vec!["https://files.grouplens.org/ml-25m.zip".to_owned()]
        );
        assert_eq!(artifact.size, Some(261_978_986));
        assert_eq!(artifact.media_type.as_deref(), Some("application/zip"));
        let digest = artifact.digest.as_ref().unwrap();
        assert!(digest.blake3.is_none());
        assert_eq!(
            digest.sha256.unwrap().to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn a_file_object_with_no_location_is_refused() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "no-location",
            "distribution": [
                {"@type": "cr:FileObject", "@id": "orphan"}
            ]
        }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(
            error
                .next_action()
                .contains("no contentUrl and no containedIn")
        );
    }

    #[test]
    fn a_size_written_as_a_rendered_quantity_is_refused() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "sized",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "big.bin",
                    "contentUrl": "https://host/big.bin",
                    "contentSize": "1.2 MB"
                }
            ]
        }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("rendered quantity"));
    }

    #[test]
    fn an_md5_digest_is_refused_by_name() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "hashed",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "file.bin",
                    "contentUrl": "https://host/file.bin",
                    "md5": "d41d8cd98f00b204e9800998ecf8427e"
                }
            ]
        }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("md5"));
    }

    #[test]
    fn a_file_set_over_one_file_object_contributes_its_includes_as_select() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "archive",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "archive.zip",
                    "contentUrl": "https://host/archive.zip",
                    "encodingFormat": "application/zip"
                },
                {
                    "@type": "cr:FileSet",
                    "@id": "csv-files",
                    "containedIn": {"@id": "archive.zip"},
                    "encodingFormat": "text/csv",
                    "includes": "*.csv"
                }
            ]
        }"#;
        let reader = Croissant;
        let manifest = read(reader, document).unwrap();
        assert_eq!(manifest.artifacts.len(), 1);
        let artifact = &manifest.artifacts[0];
        assert_eq!(artifact.id, "archive.zip");
        assert_eq!(
            artifact.sources,
            vec!["https://host/archive.zip".to_owned()]
        );
        assert_eq!(artifact.media_type.as_deref(), Some("application/zip"));
        assert_eq!(artifact.select.len(), 1);
        assert_eq!(artifact.select[0].as_str(), "*.csv");
        assert_eq!(artifact.layout, Layout::Keep);
    }

    #[test]
    fn a_file_set_excludes_list_is_refused() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "archive",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "archive.zip",
                    "contentUrl": "https://host/archive.zip"
                },
                {
                    "@type": "cr:FileSet",
                    "@id": "csv-files",
                    "containedIn": {"@id": "archive.zip"},
                    "includes": "*.csv",
                    "excludes": "*.bak"
                }
            ]
        }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("exclusion"));
    }

    #[test]
    fn a_file_set_spanning_several_containers_is_refused() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "archive",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "part1.zip",
                    "contentUrl": "https://host/part1.zip"
                },
                {
                    "@type": "cr:FileObject",
                    "@id": "part2.zip",
                    "contentUrl": "https://host/part2.zip"
                },
                {
                    "@type": "cr:FileSet",
                    "@id": "csv-files",
                    "containedIn": [{"@id": "part1.zip"}, {"@id": "part2.zip"}],
                    "includes": "*.csv"
                }
            ]
        }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("several containers"));
    }

    #[test]
    fn a_file_set_over_another_file_set_is_refused() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "archive",
            "distribution": [
                {
                    "@type": "cr:FileObject",
                    "@id": "archive.zip",
                    "contentUrl": "https://host/archive.zip"
                },
                {
                    "@type": "cr:FileSet",
                    "@id": "inner",
                    "containedIn": {"@id": "archive.zip"},
                    "includes": "*.csv"
                },
                {
                    "@type": "cr:FileSet",
                    "@id": "outer",
                    "containedIn": {"@id": "inner"},
                    "includes": "*.gz"
                }
            ]
        }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("another FileSet"));
    }

    #[test]
    fn a_malformed_document_fails_manifest_invalid_rather_than_partial() {
        let document = br#"{ "name": "broken", "distribution": "not-an-array" }"#;
        let reader = Croissant;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn artifact_ordering_is_deterministic() {
        let document = br#"{
            "@type": "cr:Dataset",
            "name": "many",
            "distribution": [
                {"@type": "cr:FileObject", "@id": "b.bin", "contentUrl": "https://host/b.bin"},
                {"@type": "cr:FileObject", "@id": "a.bin", "contentUrl": "https://host/a.bin"},
                {"@type": "cr:FileObject", "@id": "c.bin", "contentUrl": "https://host/c.bin"}
            ]
        }"#;
        let reader = Croissant;
        let first = read(reader, document).unwrap();
        let second = read(reader, document).unwrap();
        assert_eq!(first, second);
        let ids: Vec<&str> = first
            .artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b.bin", "a.bin", "c.bin"]);
    }

    #[test]
    fn a_hostile_deeply_nested_document_fails_rather_than_crashing() {
        let mut document = String::new();
        for _ in 0..10_000 {
            document.push('[');
        }
        for _ in 0..10_000 {
            document.push(']');
        }
        let reader = Croissant;
        let error = read(reader, document.as_bytes()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn recognizes_a_croissant_context() {
        let document = br#"{
            "@context": {"cr": "http://mlcommons.org/croissant/"},
            "distribution": []
        }"#;
        let reader = Croissant;
        assert!(reader.recognizes("metadata.json", document));
    }

    #[test]
    fn does_not_recognize_an_unrelated_document() {
        let document = br#"{"hello": "world"}"#;
        let reader = Croissant;
        assert!(!reader.recognizes("metadata.json", document));
    }
}
