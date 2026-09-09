//! Turning a reference the user wrote into one this build can fetch.

use fetchloom_engine::adapters::Adapters;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::seam::source::Source;

use crate::settings;

const METADATA_SCHEME: &str = "croissant:";

#[must_use]
pub(crate) fn is_metadata_document(reference: &str) -> bool {
    reference.starts_with(METADATA_SCHEME)
}

fn metadata_location(reference: &str) -> Result<String, Error> {
    let rest = reference.strip_prefix(METADATA_SCHEME).unwrap_or_default();
    if rest.is_empty() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            "write it as croissant:https://host/metadata.json, because croissant: names the document to read and this one names nothing",
        ));
    }
    Ok(rest.to_owned())
}

#[must_use]
fn has_explicit_scheme(reference: &str) -> bool {
    if reference.contains("://") || is_metadata_document(reference) {
        return true;
    }
    let Some((scheme, rest)) = reference.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && !rest.is_empty()
        && scheme.len() > 1
        && scheme
            .chars()
            .all(|letter| letter.is_ascii_lowercase() || letter.is_ascii_digit())
}

#[must_use]
fn is_name(reference: &str) -> bool {
    if has_explicit_scheme(reference) || reference.contains('\\') {
        return false;
    }
    let without_release = reference.split('@').next().unwrap_or(reference);
    !without_release.is_empty()
        && !without_release.starts_with('.')
        && !without_release.starts_with('/')
        && without_release.split('/').all(|part| {
            !part.is_empty()
                && !part.contains('.')
                && part
                    .chars()
                    .all(|letter| letter.is_ascii_alphanumeric() || letter == '-' || letter == '_')
        })
}

#[must_use]
pub fn candidates(reference: &str, sources: &[String]) -> Vec<String> {
    sources
        .iter()
        .map(|base| {
            if base.ends_with('/') || base.ends_with(':') {
                format!("{base}{reference}")
            } else {
                format!("{base}/{reference}")
            }
        })
        .collect()
}

#[must_use]
fn unmatched(reference: &str, tried: usize) -> Error {
    if tried == 0 {
        return Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "add a sources list to fetchloom.toml naming where {} is published, because a name resolves through the configured source priority and none is configured",
                SafeUrl::new(reference)
            ),
        );
    }
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name the location instead, because {} matched none of the {tried} configured sources and a name is never guessed at",
            SafeUrl::new(reference)
        ),
    )
}

pub(crate) fn resolve_reference(
    reference: &str,
    adapters: &Adapters,
    resolved: &settings::Settings,
    policy: &dyn Policy,
    work: &std::sync::Arc<fetchloom_engine::work::WorkCounter>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<String, fetchloom_engine::error::Error> {
    if fetchloom_sources::is_doi(reference) {
        let router =
            fetchloom_sources::DoiRouter::new(*policy.limits(), std::sync::Arc::clone(work));
        let credential = policy.credential(
            &fetchloom_engine::reference::Host::new("api.datacite.org".to_owned()),
            fetchloom_engine::credential::Necessity::Optional,
        )?;
        let provider = router.route(reference, credential.as_ref())?;
        observer.emit(&Event::new(
            sequence,
            EventPayload::ResolveAlias {
                from: fetchloom_engine::redact::SafeUrl::new(reference),
                to: fetchloom_engine::redact::SafeUrl::new(&provider),
            },
        ));
        return Ok(provider);
    }
    if has_explicit_scheme(reference) || !is_name(reference) {
        return Ok(reference.to_owned());
    }
    if std::path::Path::new(reference).exists() {
        return Ok(reference.to_owned());
    }
    let sources = &resolved.sources.value;
    let candidates = candidates(reference, sources);
    for candidate in &candidates {
        let Some((source, _)) = adapters.serving(candidate) else {
            continue;
        };
        let host = crate::run::host_of(candidate);
        let credential = policy.credential(
            &fetchloom_engine::reference::Host::new(host),
            fetchloom_engine::credential::Necessity::Optional,
        )?;
        if source.probe(candidate, credential.as_ref()).is_ok() {
            observer.emit(&Event::new(
                sequence,
                EventPayload::ResolveAlias {
                    from: fetchloom_engine::redact::SafeUrl::new(reference),
                    to: fetchloom_engine::redact::SafeUrl::new(candidate),
                },
            ));
            return Ok(candidate.clone());
        }
    }
    if !sources.is_empty() || fetchloom_engine::network::forbidden() {
        return Err(unmatched(reference, candidates.len()));
    }
    let found = crate::discover::searched(reference, policy, *policy.limits(), work)?;
    observer.emit(&Event::new(
        sequence,
        EventPayload::ResolveAlias {
            from: fetchloom_engine::redact::SafeUrl::new(reference),
            to: fetchloom_engine::redact::SafeUrl::new(&found),
        },
    ));
    Ok(found)
}

pub(crate) fn manifest_from_metadata(
    reference: &str,
    adapters: &Adapters,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
) -> Result<fetchloom_engine::manifest::Manifest, fetchloom_engine::error::Error> {
    use fetchloom_engine::metadata::{Context, MetadataReader, croissant};

    let location = metadata_location(reference)?;
    let bytes = read_document(&location, adapters, policy, limits)?;
    let name = crate::run::remote_name(&location);
    croissant::Croissant.read(
        &bytes,
        &Context {
            base: &location,
            name: &name,
            limits,
        },
    )
}

pub(crate) fn read_bounded_document(
    path: &std::path::Path,
    limits: &fetchloom_engine::limits::Limits,
) -> Result<Vec<u8>, fetchloom_engine::error::Error> {
    use std::io::Read as _;

    let unreadable = |reason: &dyn std::fmt::Display| {
        fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::ManifestInvalid,
            format!("make {} readable: {reason}", path.display()),
        )
    };
    let opened = std::fs::File::open(path).map_err(|reason| unreadable(&reason))?;
    let mut bytes = Vec::new();
    opened
        .take(limits.manifest_size + 1)
        .read_to_end(&mut bytes)
        .map_err(|reason| unreadable(&reason))?;
    if bytes.len() as u64 > limits.manifest_size {
        return Err(fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::ResourceLimit,
            format!(
                "publish a smaller document, because {} is larger than the {} bytes a run reads and a document is never read in part",
                path.display(),
                limits.manifest_size
            ),
        ));
    }
    Ok(bytes)
}

fn read_document(
    location: &str,
    adapters: &Adapters,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
) -> Result<Vec<u8>, fetchloom_engine::error::Error> {
    use std::io::Read as _;

    crate::run::allowed_offline(location, policy)?;
    let mut bytes = Vec::new();
    let Some((source, _)) = adapters.serving(location) else {
        return read_bounded_document(&crate::run::local_path(location)?, limits);
    };
    {
        let credential = policy.credential(
            &fetchloom_engine::reference::Host::new(crate::run::host_of(location)),
            fetchloom_engine::credential::Necessity::Optional,
        )?;
        let served = source.fetch(location, None, credential.as_ref(), None)?;
        served
            .body
            .take(limits.manifest_size + 1)
            .read_to_end(&mut bytes)
            .map_err(|reason| {
                fetchloom_engine::error::Error::new(
                    fetchloom_engine::error::ErrorKind::ManifestInvalid,
                    format!("serve the document again, because it could not be read: {reason}"),
                )
            })?;
    }
    if bytes.len() as u64 > limits.manifest_size {
        return Err(fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::ResourceLimit,
            format!(
                "publish a smaller document, because {} is larger than the {} bytes a run reads and a document is never read in part",
                fetchloom_engine::redact::SafeUrl::new(location),
                limits.manifest_size
            ),
        )
        .with_source(location));
    }
    Ok(bytes)
}

pub(crate) fn resolve_manifest(
    adapters: &Adapters,
    reference: &str,
    source: &std::path::Path,
    remote: bool,
) -> Result<(fetchloom_engine::manifest::Manifest, bool), fetchloom_engine::error::Error> {
    let read = if remote {
        None
    } else {
        crate::run::manifest_at(source).transpose()?
    };
    let is_dataset = read.is_some();
    let manifest = read.unwrap_or_else(|| {
        crate::run::synthesized_manifest(
            adapters,
            &crate::run::dataset_name(adapters, reference, source),
            reference,
        )
    });
    Ok((manifest, is_dataset))
}

#[cfg(test)]
mod tests {
    use super::{candidates, has_explicit_scheme, is_name, metadata_location, unmatched};

    #[test]
    fn a_scheme_is_recognized_and_a_windows_drive_is_not_one() {
        assert!(has_explicit_scheme("https://host/x"));
        assert!(has_explicit_scheme("hf:datasets/org/name"));
        assert!(has_explicit_scheme("croissant:https://host/m.json"));
        assert!(has_explicit_scheme("blake3:aabb"));
        assert!(!has_explicit_scheme("C:\\data\\set"));
        assert!(!has_explicit_scheme("silesia"));
        assert!(!has_explicit_scheme("./local/path"));
    }

    #[test]
    fn a_bare_name_and_a_namespaced_release_are_names() {
        assert!(is_name("silesia"));
        assert!(is_name("acme/imagenet@2012"));
        assert!(is_name("acme/imagenet"));
    }

    #[test]
    fn a_path_and_a_location_are_not_names() {
        assert!(!is_name("./data"));
        assert!(!is_name("/data/raw"));
        assert!(!is_name("data.yaml"));
        assert!(!is_name("https://host/x"));
        assert!(!is_name("C:\\data"));
        assert!(!is_name("hf:datasets/org/name"));
    }

    #[test]
    fn a_name_is_appended_to_each_base_in_order() {
        let sources = vec![
            "https://lab.edu/data/".to_owned(),
            "hf:datasets/acme".to_owned(),
        ];
        assert_eq!(
            candidates("silesia", &sources),
            vec![
                "https://lab.edu/data/silesia".to_owned(),
                "hf:datasets/acme/silesia".to_owned()
            ]
        );
    }

    #[test]
    fn a_name_with_no_sources_configured_says_to_configure_one() {
        let said = unmatched("silesia", 0).next_action().to_owned();
        assert!(said.contains("sources"), "{said}");
        assert!(said.contains("fetchloom.toml"), "{said}");
    }

    #[test]
    fn a_name_that_matched_nothing_names_how_many_were_tried() {
        let said = unmatched("silesia", 3).next_action().to_owned();
        assert!(said.contains('3'), "{said}");
        assert!(said.contains("never guessed"), "{said}");
    }

    #[test]
    fn a_metadata_document_names_the_location_after_the_scheme() {
        assert_eq!(
            metadata_location("croissant:https://host/m.json").unwrap_or_default(),
            "https://host/m.json"
        );
        assert!(metadata_location("croissant:").is_err());
    }
}
