//! kaggle: datasets by owner and name, listed at one version through kaggle's API and downloaded
//! one file at a time.

mod listing;
mod token;

use std::fmt;

use url::Url;

use super::{Var, malformed};
use crate::error::{Error, Kind};
use crate::net::Credential;

/// the host of kaggle's API, and the only host kaggle's credentials go to.
const HOST: &str = "api.kaggle.com";

/// a kaggle dataset, at a version or at its newest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    owner: String,
    slug: String,
    version: Option<u32>,
}

impl Dataset {
    /// reads the part after `kaggle:`, such as `uciml/iris` or `uciml/iris@2`.
    pub(super) fn parse(text: &str) -> Result<Dataset, Error> {
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
        if owner == "competitions" {
            return Err(competition(slug));
        }
        Ok(Dataset {
            owner: owner.to_owned(),
            slug: slug.to_owned(),
            version,
        })
    }

    /// a pasted kaggle.com dataset link, and the file in it when the link selects one.
    pub(super) fn from_link(url: &Url) -> Option<(Dataset, Option<String>)> {
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
        let file = url
            .query_pairs()
            .find(|(name, value)| name == "select" && !value.is_empty())
            .map(|(_, value)| value.into_owned());
        let dataset = Dataset {
            owner: (*owner).to_owned(),
            slug: (*slug).to_owned(),
            version,
        };
        Some((dataset, file))
    }
}

/// the refusal for a pasted link to a kaggle competition.
pub(super) fn competition_link(url: &Url) -> Option<Error> {
    match parts(url)?.as_slice() {
        ["competitions" | "c", slug, ..] if is_part(slug) => Some(competition(slug)),
        _ => None,
    }
}

/// the refusal for a kaggle competition, whose data comes with rules each person accepts on
/// kaggle.com.
fn competition(slug: &str) -> Error {
    Error::new(
        Kind::ReferenceInvalid,
        format!("{slug} is a kaggle competition, which fetchloom does not fetch"),
    )
    .help(format!("accept its rules at https://www.kaggle.com/competitions/{slug}/rules, then download it with the kaggle CLI: kaggle competitions download -c {slug}"))
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

/// an owner or a dataset name: letters, digits, `-`, `_` and `.`.
fn is_part(text: &str) -> bool {
    !text.is_empty()
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
pub(super) fn credentials(var: Var<'_>) -> Vec<Credential> {
    token::find(var)
        .map(|value| Credential {
            host: HOST,
            header: "authorization",
            value,
        })
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn link(text: &str) -> Option<(String, Option<String>)> {
        Dataset::from_link(&Url::parse(text).unwrap())
            .map(|(dataset, file)| (dataset.to_string(), file))
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
    fn competitions_are_refused_with_the_way_to_get_them() {
        for text in [
            "https://www.kaggle.com/competitions/titanic",
            "https://www.kaggle.com/c/titanic/data",
        ] {
            let err = competition_link(&Url::parse(text).unwrap()).unwrap();
            assert_eq!(err.kind, Kind::ReferenceInvalid, "{text}");
            assert!(
                err.message.contains("titanic is a kaggle competition"),
                "{text}"
            );
            let help = err.help.unwrap();
            assert!(
                help.contains("https://www.kaggle.com/competitions/titanic/rules")
                    && help.contains("kaggle competitions download -c titanic"),
                "{help}"
            );
        }
        let typed = Dataset::parse("competitions/titanic").unwrap_err();
        assert!(typed.message.contains("kaggle competition"));
        assert!(
            competition_link(&Url::parse("https://www.kaggle.com/datasets/uciml/iris").unwrap())
                .is_none()
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
                credential.host,
                credential.header,
                credential.value.as_str()
            ),
            ("api.kaggle.com", "authorization", "Bearer KGAT_abc")
        );
    }
}
