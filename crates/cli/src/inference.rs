//! Writing a manifest for data that has none.

use std::path::Path;

use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing::Digester;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::{Artifact, DigestClaims, Manifest};
use fetchloom_engine::pool::Processor;

#[must_use]
pub(crate) fn dataset_name(reference: &str) -> String {
    let trimmed = reference.trim_end_matches('/');
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    let last = after_scheme
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty() && *part != ".");
    last.map_or_else(|| "dataset".to_owned(), str::to_owned)
}

pub(crate) fn from_directory(
    root: &Path,
    processor: &Processor,
    limits: &Limits,
    into: Option<&fetchloom_cache::Cache<fetchloom_platform::NativePlatform>>,
) -> Result<Manifest, Error> {
    let walked = crate::materialize::walk(root)?;
    if !walked.links.is_empty() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "remove the {} symbolic link this directory holds, because a manifest names objects and states no link",
                walked.links.len()
            ),
        ));
    }
    let mut digester = Digester::new();
    let mut artifacts = Vec::new();
    for file in &walked.files {
        let path = walked.root.join(&file.relative);
        let (content, interop, length) = if let Some(cache) = into {
            let ingested = cache.ingest(&path)?;
            (ingested.digest, ingested.interop, ingested.size)
        } else {
            {
                let opened = std::fs::File::open(&path).map_err(|reason| {
                    Error::new(
                        ErrorKind::ReferenceUnresolved,
                        format!("make {} readable: {reason}", path.display()),
                    )
                })?;
                let digests = digester.hash(processor, opened).map_err(|reason| {
                    Error::new(
                        ErrorKind::ReferenceUnresolved,
                        format!("make {} readable: {reason}", path.display()),
                    )
                })?;
                (digests.content, digests.interop, digests.length)
            }
        };
        let id = file.entry.as_str().to_owned();
        artifacts.push(Artifact {
            sources: vec![id.clone()],
            id,
            size: Some(length),
            digest: Some(DigestClaims {
                blake3: Some(content),
                sha256: Some(interop),
            }),
            media_type: None,
            archive: None,
            select: Vec::new(),
            layout: fetchloom_engine::selection::Layout::default(),
        });
    }
    finish(dataset_name(&root.display().to_string()), artifacts, limits)
}

pub struct Observed {
    pub path: String,
    pub content: ContentDigest,
    pub interop: Option<InteropDigest>,
    pub size: u64,
}

pub(crate) fn from_observed(
    location: &str,
    observed: &[Observed],
    limits: &Limits,
) -> Result<Manifest, Error> {
    let artifacts = observed
        .iter()
        .map(|entry| Artifact {
            id: entry.path.clone(),
            sources: vec![fetchloom_sources::joined(location, &entry.path)],
            size: Some(entry.size),
            digest: Some(DigestClaims {
                blake3: Some(entry.content),
                sha256: entry.interop,
            }),
            media_type: None,
            archive: None,
            select: Vec::new(),
            layout: fetchloom_engine::selection::Layout::default(),
        })
        .collect();
    finish(dataset_name(location), artifacts, limits)
}

fn finish(name: String, mut artifacts: Vec<Artifact>, limits: &Limits) -> Result<Manifest, Error> {
    if artifacts.len() as u64 > limits.listing_entries {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "narrow the reference, because it holds {} entries past the limit of {}",
                artifacts.len(),
                limits.listing_entries
            ),
        ));
    }
    if artifacts.is_empty() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            "point init at something holding at least one file, because a manifest naming no artifact resolves to nothing",
        ));
    }
    artifacts.sort_by(|one, other| one.id.as_bytes().cmp(other.id.as_bytes()));
    let manifest = Manifest {
        name,
        release: None,
        artifacts,
        license: None,
        derived_from: None,
    };
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::dataset_name;

    #[test]
    fn a_name_is_the_last_named_part_of_a_reference() {
        assert_eq!(dataset_name("https://host/set/"), "set");
        assert_eq!(dataset_name("./raw"), "raw");
        assert_eq!(dataset_name("."), "dataset");
    }
}
