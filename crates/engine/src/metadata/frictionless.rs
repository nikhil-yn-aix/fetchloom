//! Reading a manifest out of a Frictionless data package descriptor.

use serde_json::{Map, Value};

use crate::digest::InteropDigest;
use crate::document::{self, Syntax};
use crate::error::{Error, ErrorKind};
use crate::license::License;
use crate::manifest::{Artifact, DigestClaims, Manifest};
use crate::metadata::{Context, MetadataFormat, MetadataReader, malformed, unrepresentable};
use crate::selection::Layout;

#[derive(Clone, Copy, Debug, Default)]
pub struct FrictionlessPackage;

impl MetadataReader for FrictionlessPackage {
    fn format(&self) -> MetadataFormat {
        MetadataFormat::FrictionlessPackage
    }

    fn recognizes(&self, name: &str, bytes: &[u8]) -> bool {
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(bytes) else {
            return false;
        };
        if !fields.get("resources").is_some_and(Value::is_array) {
            return false;
        }
        let profile_names_data_package = fields
            .get("profile")
            .and_then(Value::as_str)
            .is_some_and(|profile| profile.contains("data-package"));
        let basename = name.rsplit(['/', '\\']).next().unwrap_or(name);
        profile_names_data_package || basename == "datapackage.json"
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
        let license = read_license(root)?;
        let resources = root
            .get("resources")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed(self.format(), "name a resources array"))?;

        if resources.len() as u64 > context.limits.listing_entries {
            return Err(Error::new(
                ErrorKind::ResourceLimit,
                format!(
                    "reduce resources to at most {} entries, because this document names {}",
                    context.limits.listing_entries,
                    resources.len()
                ),
            ));
        }

        let mut artifacts = Vec::with_capacity(resources.len());
        for resource in resources {
            let Value::Object(fields) = resource else {
                return Err(malformed(self.format(), "each resource is a JSON object"));
            };
            artifacts.push(read_resource(fields)?);
        }
        if artifacts.is_empty() {
            return Err(malformed(self.format(), "name at least one resource"));
        }

        Ok(Manifest {
            name,
            release,
            artifacts,
            license,
        })
    }
}

fn read_license(root: &Map<String, Value>) -> Result<Option<License>, Error> {
    let Some(licenses) = root.get("licenses").and_then(Value::as_array) else {
        return Ok(None);
    };
    let Some(first) = licenses.first() else {
        return Ok(None);
    };
    let Value::Object(fields) = first else {
        return Err(malformed(
            MetadataFormat::FrictionlessPackage,
            "each license is a JSON object",
        ));
    };
    let spdx = fields
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let url = fields
        .get("path")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(Some(License {
        spdx,
        url,
        requires_acceptance: false,
    }))
}

fn read_resource(fields: &Map<String, Value>) -> Result<Artifact, Error> {
    let id = fields
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            malformed(
                MetadataFormat::FrictionlessPackage,
                "give each resource a name",
            )
        })?
        .to_owned();

    let path = fields.get("path");
    if let Some(Value::Array(_)) = path {
        return Err(unrepresentable(
            "a Frictionless multipart resource path",
            "path is an array of several files concatenated into one logical resource, and the \
             manifest names one object per artifact",
        ));
    }

    if fields.get("data").is_some() && path.is_none() {
        return Err(unrepresentable(
            "a Frictionless resource carrying inline data",
            "the resource states its bytes as inline data in the descriptor, and there is \
             nothing to fetch",
        ));
    }

    let mut sources = Vec::new();
    if let Some(text) = path.and_then(Value::as_str) {
        sources.push(text.to_owned());
    } else if let Some(text) = fields.get("url").and_then(Value::as_str) {
        sources.push(text.to_owned());
    }
    if sources.is_empty() {
        return Err(unrepresentable(
            "a Frictionless resource naming no path or url",
            "the resource states no path and no url, so it names no location",
        ));
    }

    let size = match fields.get("bytes") {
        None => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| {
            malformed(
                MetadataFormat::FrictionlessPackage,
                "bytes is a whole number",
            )
        })?),
    };

    let media_type = fields
        .get("mediatype")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let digest = match fields.get("hash").and_then(Value::as_str) {
        None => None,
        Some(hash) => Some(read_hash(hash)?),
    };

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

fn read_hash(hash: &str) -> Result<DigestClaims, Error> {
    if let Some(hex) = hash.strip_prefix("sha256:") {
        let interop: InteropDigest = format!("sha256:{hex}").parse().map_err(|_| {
            malformed(
                MetadataFormat::FrictionlessPackage,
                "sha256 is sixty-four lowercase hexadecimal characters",
            )
        })?;
        return Ok(DigestClaims {
            blake3: None,
            sha256: Some(interop),
        });
    }
    if let Some((algorithm, _)) = hash.split_once(':') {
        return Err(unrepresentable(
            "a Frictionless hash algorithm",
            &format!(
                "the hash is prefixed {algorithm}, which this build does not carry a slot for"
            ),
        ));
    }
    Err(unrepresentable(
        "a Frictionless bare hash value",
        "a hash with no algorithm prefix is MD5 by the Frictionless default, which this build \
         does not carry a slot for",
    ))
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "test setup, where a failure to build the input is the assertion"
    )]

    use super::FrictionlessPackage;
    use crate::error::ErrorKind;
    use crate::limits::Limits;
    use crate::metadata::{Context, MetadataReader};

    fn read(
        reader: FrictionlessPackage,
        bytes: &[u8],
    ) -> Result<crate::manifest::Manifest, crate::error::Error> {
        let limits = Limits::default();
        reader.read(
            bytes,
            &Context {
                base: "https://example.org/",
                name: "fallback",
                limits: &limits,
            },
        )
    }

    #[test]
    fn a_well_formed_document_reads_into_the_expected_manifest() {
        let document = br#"{
            "name": "iris",
            "version": "1.0.0",
            "licenses": [{"name": "CC-BY-4.0", "path": "https://creativecommons.org/licenses/by/4.0/"}],
            "resources": [
                {
                    "name": "iris-data",
                    "path": "data/iris.csv",
                    "bytes": 4551,
                    "mediatype": "text/csv",
                    "hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                }
            ]
        }"#;
        let reader = FrictionlessPackage;
        let manifest = read(reader, document).unwrap();
        assert_eq!(manifest.name, "iris");
        assert_eq!(manifest.release.as_deref(), Some("1.0.0"));
        let license = manifest.license.unwrap();
        assert_eq!(license.spdx.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(
            license.url.as_deref(),
            Some("https://creativecommons.org/licenses/by/4.0/")
        );
        assert_eq!(manifest.artifacts.len(), 1);
        let artifact = &manifest.artifacts[0];
        assert_eq!(artifact.id, "iris-data");
        assert_eq!(artifact.sources, vec!["data/iris.csv".to_owned()]);
        assert_eq!(artifact.size, Some(4551));
        assert_eq!(artifact.media_type.as_deref(), Some("text/csv"));
        let digest = artifact.digest.as_ref().unwrap();
        assert!(digest.blake3.is_none());
        assert_eq!(
            digest.sha256.unwrap().to_string(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn a_bare_hash_value_is_refused_as_md5() {
        let document = br#"{
            "name": "iris",
            "resources": [
                {"name": "iris-data", "path": "data/iris.csv", "hash": "d41d8cd98f00b204e9800998ecf8427e"}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("MD5"));
    }

    #[test]
    fn a_prefixed_md5_hash_is_refused_by_name() {
        let document = br#"{
            "name": "iris",
            "resources": [
                {"name": "iris-data", "path": "data/iris.csv", "hash": "md5:d41d8cd98f00b204e9800998ecf8427e"}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("md5"));
    }

    #[test]
    fn a_resource_with_no_name_is_malformed() {
        let document = br#"{
            "name": "iris",
            "resources": [
                {"path": "data/iris.csv"}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn a_multipart_path_array_is_refused() {
        let document = br#"{
            "name": "iris",
            "resources": [
                {"name": "iris-data", "path": ["a.csv", "b.csv"]}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("multipart"));
    }

    #[test]
    fn inline_data_with_no_path_is_refused() {
        let document = br#"{
            "name": "iris",
            "resources": [
                {"name": "iris-data", "data": [1, 2, 3]}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("inline data"));
    }

    #[test]
    fn a_resource_naming_no_path_or_url_is_refused() {
        let document = br#"{
            "name": "iris",
            "resources": [
                {"name": "iris-data"}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
        assert!(error.next_action().contains("no location"));
    }

    #[test]
    fn a_malformed_document_fails_manifest_invalid_rather_than_partial() {
        let document = br#"{ "name": "broken", "resources": "not-an-array" }"#;
        let reader = FrictionlessPackage;
        let error = read(reader, document).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn artifact_ordering_is_deterministic() {
        let document = br#"{
            "name": "many",
            "resources": [
                {"name": "b", "path": "b.csv"},
                {"name": "a", "path": "a.csv"},
                {"name": "c", "path": "c.csv"}
            ]
        }"#;
        let reader = FrictionlessPackage;
        let first = read(reader, document).unwrap();
        let second = read(reader, document).unwrap();
        assert_eq!(first, second);
        let ids: Vec<&str> = first
            .artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
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
        let reader = FrictionlessPackage;
        let error = read(reader, document.as_bytes()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ManifestInvalid);
    }

    #[test]
    fn recognizes_a_datapackage_by_profile() {
        let document = br#"{"profile": "tabular-data-package", "resources": []}"#;
        let reader = FrictionlessPackage;
        assert!(reader.recognizes("metadata.json", document));
    }

    #[test]
    fn recognizes_a_datapackage_by_name() {
        let document = br#"{"resources": []}"#;
        let reader = FrictionlessPackage;
        assert!(reader.recognizes("datapackage.json", document));
    }

    #[test]
    fn does_not_recognize_an_unrelated_document() {
        let document = br#"{"hello": "world"}"#;
        let reader = FrictionlessPackage;
        assert!(!reader.recognizes("metadata.json", document));
    }
}
