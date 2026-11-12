//! kaggle: datasets by owner and name, listed at one version through kaggle's API, and
//! competitions by name, listed for a signed-in account. both download one file at a time.

mod competition;
mod listing;
mod token;

use std::fmt;

use url::Url;

pub(crate) use self::competition::Competition;
pub(super) use self::competition::{may_wrap, refused_download};
pub(crate) use self::token::{config_folder, locate};
use super::{Reference, Var, malformed};
use crate::error::{Error, Kind};
use crate::net::Credential;

/// the host of kaggle's API, and the only host kaggle's credentials go to.
const HOST: &str = "api.kaggle.com";

/// reads the part after `kaggle:`: a dataset such as `uciml/iris@2`, or a competition such as
/// `competitions/titanic`.
pub(super) fn parse(text: &str) -> Result<Reference, Error> {
    match text.trim().split_once('/') {
        Some((owner, slug)) if owner.eq_ignore_ascii_case("competitions") => {
            Competition::parse(slug).map(Reference::KaggleCompetition)
        }
        _ => Dataset::parse(text).map(Reference::Kaggle),
    }
}

/// a pasted kaggle.com link to a dataset or a competition, and the file in it when the link
/// selects one.
pub(super) fn from_link(url: &Url) -> Option<(Reference, Option<String>)> {
    Dataset::from_link(url)
        .map(|(dataset, file)| (Reference::Kaggle(dataset), file))
        .or_else(|| {
            Competition::from_link(url)
                .map(|(competition, file)| (Reference::KaggleCompetition(competition), file))
        })
}

/// the file a kaggle.com link selects with `?select=`, as its data pages do.
fn selected(url: &Url) -> Option<String> {
    url.query_pairs()
        .find(|(name, value)| name == "select" && !value.is_empty())
        .map(|(_, value)| value.into_owned())
}

/// the short link a kaggle DOI names, such as `https://www.kaggle.com/dsv/2736749`, which
/// redirects to one version of a dataset.
pub(super) fn version_link(text: &str) -> Option<Url> {
    let url = Url::parse(text).ok()?;
    let parts = parts(&url)?;
    let ["dsv", number] = parts.as_slice() else {
        return None;
    };
    let number: u64 = number.parse().ok()?;
    Url::parse(&format!("https://www.kaggle.com/dsv/{number}")).ok()
}

/// a kaggle dataset, at a version or at its newest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    owner: String,
    slug: String,
    version: Option<u32>,
}

impl Dataset {
    /// whether this names one version of the dataset, whose files kaggle never changes.
    pub(super) fn is_versioned(&self) -> bool {
        self.version.is_some()
    }

    /// reads a dataset after `kaggle:`, such as `uciml/iris` or `uciml/iris@2`.
    fn parse(text: &str) -> Result<Dataset, Error> {
        let bad = || {
            malformed(
                &format!("kaggle:{text}"),
                "kaggle:owner/dataset, with @version to pin one",
            )
        };
        let text = text.trim();
        let (path, version) = match text.split_once('@') {
            Some((path, version)) => (path, Some(version.parse().map_err(|_| bad())?)),
            None => (text, None),
        };
        let (owner, slug) = path.split_once('/').ok_or_else(bad)?;
        if !is_part(owner) || !is_part(slug) {
            return Err(bad());
        }
        Ok(Dataset {
            owner: owner.to_owned(),
            slug: slug.to_owned(),
            version,
        })
    }

    /// the dataset's owner and name, such as `uciml` and `iris`.
    pub(crate) fn parts(&self) -> (&str, &str) {
        (&self.owner, &self.slug)
    }

    /// a pasted kaggle.com dataset link, and the file in it when the link selects one.
    fn from_link(url: &Url) -> Option<(Dataset, Option<String>)> {
        let parts = parts(url)?;
        let ["datasets", owner, slug, rest @ ..] = parts.as_slice() else {
            return None;
        };
        if !is_part(owner) || !is_part(slug) {
            return None;
        }
        let version = match rest {
            ["versions", version, ..] => Some(version.parse().ok()?),
            _ => None,
        };
        let dataset = Dataset {
            owner: (*owner).to_owned(),
            slug: (*slug).to_owned(),
            version,
        };
        Some((dataset, selected(url)))
    }
}

/// the parts of a kaggle.com link's path.
fn parts(url: &Url) -> Option<Vec<&str>> {
    if !matches!(url.host_str()?, "kaggle.com" | "www.kaggle.com") {
        return None;
    }
    Some(
        url.path_segments()?
            .filter(|part| !part.is_empty())
            .collect(),
    )
}

/// the error for a listing of `listed`, a dataset or a competition, that fetchloom cannot use.
fn unusable(listed: impl fmt::Display, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!("kaggle sent a listing of {listed} that fetchloom cannot use: {reason}"),
    )
    .help(crate::error::report_answer())
}

/// an owner or a dataset name: letters, digits, `-`, `_` and `.`, but not only dots, which would
/// move the api link to another endpoint.
fn is_part(text: &str) -> bool {
    !text.is_empty()
        && !text.chars().all(|c| c == '.')
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kaggle:{}/{}", self.owner, self.slug)?;
        if let Some(version) = self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

/// kaggle's API token or legacy key, sent to kaggle's API and nowhere else.
pub(crate) fn credentials(var: Var<'_>) -> Vec<Credential> {
    token::find(var)
        .map(|value| Credential {
            host: HOST.to_owned(),
            header: "authorization",
            value,
            every_hop: false,
        })
        .into_iter()
        .collect()
}

/// the `authorization` value for a kaggle key a person pasted, and the host it goes to.
pub(crate) fn pasted(secret: &str) -> Option<(&'static str, String)> {
    token::pasted(secret).map(|value| (HOST, value))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::error::Kind;

    fn link(text: &str) -> Option<(String, Option<String>)> {
        Dataset::from_link(&Url::parse(text).unwrap())
            .map(|(dataset, file)| (dataset.to_string(), file))
    }

    /// a listing fetchloom cannot use names the dataset or the competition kaggle listed, why,
    /// and where to report it.
    #[test]
    fn an_unusable_listing_names_what_kaggle_listed() {
        let dataset = Dataset::parse("uciml/iris").unwrap();
        let competition = Competition::parse("titanic").unwrap();
        let cases = [
            (
                unusable(&dataset, "it sent a page twice"),
                "kaggle:uciml/iris",
            ),
            (
                unusable(&competition, "it sent a page twice"),
                "kaggle:competitions/titanic",
            ),
        ];
        for (error, listed) in cases {
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert_eq!(
                error.message,
                format!(
                    "kaggle sent a listing of {listed} that fetchloom cannot use: it sent a page twice"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(crate::error::report_answer().as_str())
            );
        }
    }

    #[test]
    fn dataset_links_name_the_dataset_its_version_and_a_selected_file() {
        let cases = [
            (
                "https://www.kaggle.com/datasets/uciml/iris",
                "kaggle:uciml/iris",
                None,
            ),
            (
                "https://kaggle.com/datasets/uciml/iris/data",
                "kaggle:uciml/iris",
                None,
            ),
            (
                "https://www.kaggle.com/datasets/uciml/iris/versions/2",
                "kaggle:uciml/iris@2",
                None,
            ),
            (
                "https://www.kaggle.com/datasets/uciml/iris/data?select=Iris.csv",
                "kaggle:uciml/iris",
                Some("Iris.csv"),
            ),
        ];
        for (text, reference, file) in cases {
            assert_eq!(
                link(text),
                Some((reference.to_owned(), file.map(str::to_owned))),
                "{text}"
            );
        }
        for other in [
            "https://www.kaggle.com/code/uciml/some-notebook",
            "https://www.kaggle.com/models/google/gemma",
            "https://www.kaggle.com/uciml/iris",
            "https://www.kaggle.com/datasets/uciml/iris/versions/latest",
        ] {
            assert_eq!(link(other), None, "{other}");
        }
    }

    #[test]
    fn competitions_are_read_from_ids_and_links() {
        let reference = |text: &str| match Reference::parse(text).unwrap() {
            crate::source::Parsed {
                reference: Reference::KaggleCompetition(competition),
                file,
                ..
            } => (competition.to_string(), file),
            other => panic!("{text}: {other:?}"),
        };
        let titanic = || ("kaggle:competitions/titanic".to_owned(), None);
        for text in [
            "kaggle:competitions/titanic",
            "kaggle:Competitions/titanic",
            "https://www.kaggle.com/competitions/titanic",
            "https://www.kaggle.com/competitions/titanic/data",
            "https://kaggle.com/c/titanic",
            "https://www.kaggle.com/c/titanic/data",
        ] {
            assert_eq!(reference(text), titanic(), "{text}");
        }
        assert_eq!(
            reference("https://www.kaggle.com/competitions/titanic/data?select=train.csv"),
            (
                "kaggle:competitions/titanic".to_owned(),
                Some("train.csv".to_owned())
            )
        );
        for bad in [
            "kaggle:competitions/titanic@2",
            "kaggle:competitions/",
            "kaggle:competitions/..",
            "kaggle:competitions/titanic/data",
            "kaggle:../iris",
            "kaggle:uciml/..",
            "kaggle:./.",
        ] {
            let err = Reference::parse(bad).unwrap_err();
            assert_eq!(err.kind, Kind::ReferenceInvalid, "{bad}");
        }
        assert!(
            Reference::parse("kaggle:competitions/titanic@2")
                .unwrap_err()
                .help
                .unwrap()
                .contains("no versions")
        );
        assert!(
            Reference::parse("https://www.kaggle.com/competitions")
                .unwrap_err()
                .message
                .contains("kaggle link")
        );
    }

    #[test]
    fn credentials_go_to_the_api_host_only() {
        let var = |name: &str| (name == "KAGGLE_API_TOKEN").then(|| OsString::from("KGAT_abc"));
        let found = credentials(&var);
        let [credential] = found.as_slice() else {
            panic!("{} credentials", found.len());
        };
        assert_eq!(
            (
                credential.host.as_str(),
                credential.header,
                credential.value.as_str()
            ),
            ("api.kaggle.com", "authorization", "Bearer KGAT_abc")
        );
    }
}
