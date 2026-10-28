//! kaggle: datasets by owner and name, optionally pinned to a version, listed and downloaded
//! through kaggle's public API.

use std::fmt;

use url::Url;

use super::{Listing, Var, malformed};
use crate::error::Error;
use crate::net::{Api, Credential};

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
        Ok(Dataset {
            owner: owner.to_owned(),
            slug: slug.to_owned(),
            version,
        })
    }

    /// a pasted kaggle.com dataset link.
    pub(super) fn from_link(url: &Url) -> Option<Dataset> {
        if !matches!(url.host_str()?, "kaggle.com" | "www.kaggle.com") {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let [kind, owner, slug, rest @ ..] = parts.as_slice() else {
            return None;
        };
        if *kind != "datasets" || !is_part(owner) || !is_part(slug) {
            return None;
        }
        let version = match rest {
            ["versions", version, ..] => Some(version.parse().ok()?),
            _ => None,
        };
        Some(Dataset {
            owner: (*owner).to_owned(),
            slug: (*slug).to_owned(),
            version,
        })
    }

    /// lists the dataset's files through kaggle's API.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the listing arrives with the kaggle milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        Err(super::not_yet("kaggle"))
    }
}

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

/// kaggle's API key, from where the kaggle tools keep it.
pub(super) fn credentials(_var: Var<'_>) -> Vec<Credential> {
    Vec::new()
}
