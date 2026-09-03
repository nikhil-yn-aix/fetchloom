//! Turning a reference the user wrote into one this build can fetch.

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;

/// The prefix a metadata document reference carries.
const METADATA_SCHEME: &str = "croissant:";

/// What a reference names, once the resolution order has been walked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolved {
    /// A location an adapter serves, or a path on this machine.
    Direct(String),
    /// A metadata document to read a manifest out of.
    MetadataDocument(String),
}

/// Reports whether a reference names a metadata document.
#[must_use]
pub fn is_metadata_document(reference: &str) -> bool {
    reference.starts_with(METADATA_SCHEME)
}

/// Returns the location a metadata document reference names.
///
/// # Errors
///
/// Fails with `reference.unresolved` when the reference names no location after
/// the scheme.
pub fn metadata_location(reference: &str) -> Result<String, Error> {
    let rest = reference.strip_prefix(METADATA_SCHEME).unwrap_or_default();
    if rest.is_empty() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            "write it as croissant:https://host/metadata.json, because croissant: names the document to read and this one names nothing",
        ));
    }
    Ok(rest.to_owned())
}

/// Reports whether a reference carries a scheme, which the resolution order
/// settles before anything else is tried.
#[must_use]
pub fn has_explicit_scheme(reference: &str) -> bool {
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

/// Reports whether a reference is a bare name or a namespaced release, which
/// are the two forms the configured source priority resolves.
#[must_use]
pub fn is_name(reference: &str) -> bool {
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

/// Returns each location a name resolves to, in the order the configuration
/// gave them.
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

/// Returns the failure a name that matched nothing states.
#[must_use]
pub fn unmatched(reference: &str, tried: usize) -> Error {
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
