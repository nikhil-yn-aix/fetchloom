//! Writing a manifest for data that has none.

use std::path::Path;

use fetchloom_engine::digest::{ContentDigest, InteropDigest};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing::Digester;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::{Artifact, DigestClaims, Manifest};
use fetchloom_engine::pool::Processor;

/// Returns the dataset name a reference carries, which is the last named part
/// of it.
#[must_use]
pub fn dataset_name(reference: &str) -> String {
    let trimmed = reference.trim_end_matches('/');
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    let last = after_scheme
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty() && *part != ".");
    last.map_or_else(|| "dataset".to_owned(), str::to_owned)
}

/// Infers a manifest for a directory on this machine, reading every file once
/// to record the digests it observed.
///
/// # Errors
///
/// Fails when the directory cannot be walked or a file cannot be read.
pub fn from_directory(
    root: &Path,
    processor: &Processor,
    limits: &Limits,
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
        let id = file.entry.as_str().to_owned();
        artifacts.push(Artifact {
            sources: vec![id.clone()],
            id,
            size: Some(digests.length),
            digest: Some(DigestClaims {
                blake3: Some(digests.content),
                sha256: Some(digests.interop),
            }),
            media_type: None,
            archive: None,
            select: Vec::new(),
            layout: fetchloom_engine::selection::Layout::default(),
        });
    }
    finish(dataset_name(&root.display().to_string()), artifacts, limits)
}

/// What one entry of a container hashed to when inference read its bytes.
pub struct Observed {
    /// The entry's path relative to the container, which is its identifier.
    pub path: String,
    /// The content digest of the bytes that arrived.
    pub content: ContentDigest,
    /// The interop digest of the same bytes, when the store recorded it.
    pub interop: Option<InteropDigest>,
    /// How many bytes arrived.
    pub size: u64,
}

/// Infers a manifest for a container reached over the network, from the bytes
/// inference actually read rather than from anything a metadata request said.
///
/// # Errors
///
/// Fails when the container holds more entries than the limit permits, or none.
pub fn from_observed(
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

/// Orders the artifacts, bounds them, and refuses a manifest that names none.
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
