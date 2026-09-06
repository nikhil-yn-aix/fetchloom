//! Resolving a bare name by searching the registries that offer search.

use std::fmt::Write as _;
use std::sync::Arc;

use fetchloom_engine::credential::Necessity;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::work::WorkCounter;
use fetchloom_sources::{Found, Registry, Searcher};

const NEAREST: usize = 5;
const A_NAME_IS_NEAR_WITHIN: usize = 3;

#[must_use]
fn folded(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|letter| letter.to_ascii_lowercase())
        .collect()
}

#[must_use]
fn distance(one: &str, other: &str) -> usize {
    let one: Vec<char> = one.chars().collect();
    let other: Vec<char> = other.chars().collect();
    let mut previous: Vec<usize> = (0..=other.len()).collect();
    let mut current = vec![0; other.len() + 1];
    for (row, letter) in one.iter().enumerate() {
        current[0] = row + 1;
        for (column, against) in other.iter().enumerate() {
            let substitution = previous[column] + usize::from(letter != against);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[other.len()]
}

#[must_use]
fn is_the_name(term: &str, found: &Found) -> bool {
    folded(&found.name) == folded(term)
}

#[must_use]
fn evidence(registry: Registry) -> &'static str {
    match registry {
        Registry::Dataverse => "a checksum the install states, verified when it is SHA-256",
        Registry::Ckan => "a checksum only where the install writes one, so usually none",
        Registry::HuggingFace
        | Registry::Kaggle
        | Registry::OpenMl
        | Registry::Zenodo
        | Registry::Figshare
        | Registry::DataCite => "no checksum, so a first fetch is trusted on first use",
    }
}

#[must_use]
fn sized(size: Option<u64>) -> String {
    match size {
        Some(bytes) => crate::observer::human_bytes(bytes),
        None => "an unstated size".to_owned(),
    }
}

fn several(term: &str, matched: &[Found]) -> Error {
    let mut said = format!(
        "name one of them, because {term} matched {} records and a name is never guessed at:",
        matched.len()
    );
    for found in matched {
        let _ = write!(
            said,
            "\n  {} — {}, {}, {}\n    fetchloom get {}",
            found.reference,
            found.title,
            sized(found.size),
            evidence(found.registry),
            found.reference
        );
    }
    Error::new(ErrorKind::ReferenceUnresolved, said).with_source(term)
}

fn nothing(term: &str, searched: usize, every: &[Found]) -> Error {
    let mut near: Vec<(usize, &Found)> = every
        .iter()
        .map(|found| (distance(&folded(&found.name), &folded(term)), found))
        .filter(|(apart, _)| *apart <= A_NAME_IS_NEAR_WITHIN)
        .collect();
    near.sort_by(|one, other| {
        one.0
            .cmp(&other.0)
            .then_with(|| one.1.reference.cmp(&other.1.reference))
    });
    near.dedup_by(|one, other| one.1.name == other.1.name);
    if near.is_empty() {
        return Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name the location instead, because {term} matched nothing in the {searched} registries this build searches and a name is never guessed at"
            ),
        )
        .with_source(term);
    }
    let mut said = format!("did you mean one of these, because {term} matched nothing exactly:");
    for (_, found) in near.iter().take(NEAREST) {
        let _ = write!(
            said,
            "\n  {}\n    fetchloom get {}",
            found.name, found.reference
        );
    }
    Error::new(ErrorKind::ReferenceUnresolved, said).with_source(term)
}

/// # Errors
/// `reference.unresolved` naming every match when a term matched more than
/// one, and naming the nearest names it did find when it matched none.
pub(crate) fn decided(term: &str, searched: usize, every: &[Found]) -> Result<String, Error> {
    let mut matched: Vec<Found> = every
        .iter()
        .filter(|found| is_the_name(term, found))
        .cloned()
        .collect();
    matched.sort_by(|one, other| one.reference.cmp(&other.reference));
    matched.dedup_by(|one, other| one.reference == other.reference);
    match matched.len() {
        0 => Err(nothing(term, searched, every)),
        1 => Ok(matched
            .into_iter()
            .next()
            .map_or_else(String::new, |found| found.reference)),
        _ => Err(several(term, &matched)),
    }
}

pub(crate) fn searched(
    term: &str,
    policy: &dyn Policy,
    limits: Limits,
    work: &Arc<WorkCounter>,
) -> Result<String, Error> {
    let registries = Registry::ALL;
    let mut every = Vec::new();
    std::thread::scope(|scope| {
        let running: Vec<_> = registries
            .iter()
            .map(|registry| {
                let work = Arc::clone(work);
                scope.spawn(move || {
                    let credential = policy
                        .credential(&Host::new(registry.host().to_owned()), Necessity::Optional)
                        .ok()
                        .flatten();
                    Searcher::new(*registry, limits, work).search(term, credential.as_ref())
                })
            })
            .collect();
        for held in running {
            if let Ok(Ok(found)) = held.join() {
                every.extend(found);
            }
        }
    });
    decided(term, registries.len(), &every)
}

#[cfg(test)]
mod tests {
    use super::{decided, distance, folded};
    use fetchloom_sources::{Found, Registry};

    fn found(registry: Registry, reference: &str, name: &str, size: Option<u64>) -> Found {
        Found {
            registry,
            reference: reference.to_owned(),
            name: name.to_owned(),
            title: name.to_owned(),
            size,
        }
    }

    #[test]
    fn one_record_carrying_the_name_resolves_to_the_reference_it_names() {
        let resolved = decided(
            "ham10000",
            8,
            &[
                found(
                    Registry::Kaggle,
                    "kaggle:kmader/ham10000",
                    "HAM10000",
                    Some(10),
                ),
                found(
                    Registry::Zenodo,
                    "zenodo:10.5281/zenodo.1",
                    "melanoma",
                    None,
                ),
            ],
        );
        assert_eq!(resolved.unwrap_or_default(), "kaggle:kmader/ham10000");
    }

    #[test]
    fn two_records_carrying_the_name_are_printed_with_the_command_for_each_and_refused() {
        let refused = decided(
            "ham10000",
            8,
            &[
                found(
                    Registry::Kaggle,
                    "kaggle:kmader/ham10000",
                    "HAM10000",
                    Some(5_582_914_511),
                ),
                found(
                    Registry::Dataverse,
                    "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/LZJTKO",
                    "ham10000",
                    None,
                ),
            ],
        )
        .err();
        let Some(refused) = refused else {
            unreachable!("two records carrying one name resolved to one of them")
        };
        let said = refused.next_action();
        assert!(
            said.contains("fetchloom get kaggle:kmader/ham10000"),
            "{said}"
        );
        assert!(
            said.contains("fetchloom get dataverse:dataverse.harvard.edu/doi:10.7910/DVN/LZJTKO"),
            "{said}"
        );
        assert!(said.contains("5.2 GiB") || said.contains("GiB"), "{said}");
        assert!(said.contains("trusted on first use"), "{said}");
        assert!(said.contains("checksum the install states"), "{said}");
    }

    #[test]
    fn one_record_found_twice_under_the_same_reference_is_one_match() {
        let resolved = decided(
            "iris",
            8,
            &[
                found(Registry::OpenMl, "openml:61", "iris", None),
                found(Registry::OpenMl, "openml:61", "iris", None),
            ],
        );
        assert_eq!(resolved.unwrap_or_default(), "openml:61");
    }

    #[test]
    fn a_name_matching_nothing_names_the_nearest_it_did_find() {
        let refused = decided(
            "ham10k",
            8,
            &[found(
                Registry::Kaggle,
                "kaggle:kmader/ham10000",
                "ham10000",
                None,
            )],
        )
        .err();
        let Some(refused) = refused else {
            unreachable!("a name that matched nothing exactly resolved to something")
        };
        let said = refused.next_action();
        assert!(said.contains("did you mean"), "{said}");
        assert!(said.contains("ham10000"), "{said}");
        assert!(
            said.contains("fetchloom get kaggle:kmader/ham10000"),
            "{said}"
        );
    }

    #[test]
    fn a_name_nothing_resembles_says_how_many_registries_were_searched() {
        let refused = decided(
            "zzzqqqnothing",
            8,
            &[found(Registry::Kaggle, "kaggle:a/b", "unrelated", None)],
        )
        .err();
        let Some(refused) = refused else {
            unreachable!("a name nothing resembles resolved to something")
        };
        let said = refused.next_action();
        assert!(said.contains('8'), "{said}");
        assert!(said.contains("never guessed at"), "{said}");
        assert!(!said.contains("did you mean"), "{said}");
    }

    #[test]
    fn a_name_is_matched_without_its_case_or_its_punctuation() {
        assert_eq!(folded("HAM-10000"), "ham10000");
        assert_eq!(folded("Skin_Cancer"), "skincancer");
    }

    #[test]
    fn the_distance_between_two_names_counts_the_edits_between_them() {
        assert_eq!(distance("ham10k", "ham10000"), 3);
        assert_eq!(distance("iris", "iris"), 0);
        assert_eq!(distance("", "abc"), 3);
    }
}
