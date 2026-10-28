//! figshare: articles by id, optionally pinned to a version, listed through the articles API.

use std::fmt;

use url::Url;

use super::{Listing, Var, malformed};
use crate::error::Error;
use crate::net::{Api, Credential};

/// a figshare article, at a version or at its newest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Article {
    id: u64,
    version: Option<u32>,
}

impl Article {
    /// reads the part after `figshare:`, such as `34744689` or `34744689@2`.
    pub(super) fn parse(text: &str) -> Result<Article, Error> {
        let bad = || {
            malformed(
                &format!("figshare:{text}"),
                "figshare:34744689 or figshare:34744689@2",
            )
        };
        let (id, version) = match text.trim().split_once('@') {
            Some((id, version)) => (id, Some(version.parse().map_err(|_| bad())?)),
            None => (text.trim(), None),
        };
        Ok(Article {
            id: id.parse().map_err(|_| bad())?,
            version,
        })
    }

    /// a pasted article link, from figshare.com or an institution's figshare portal.
    pub(super) fn from_link(url: &Url) -> Option<Article> {
        let host = url.host_str()?;
        let figshare = host == "figshare.com"
            || (host.ends_with(".figshare.com")
                && !matches!(host, "ndownloader.figshare.com" | "api.figshare.com"));
        if !figshare {
            return None;
        }
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        let after = parts.iter().position(|part| *part == "articles")? + 1;
        let numbers: Vec<u64> = parts[after..]
            .iter()
            .rev()
            .map_while(|part| part.parse().ok())
            .collect();
        match numbers.as_slice() {
            // a link ends in the article id, or in the id and then a version number
            [version, id, ..] if *version < 10_000 => Some(Article {
                id: *id,
                version: Some(u32::try_from(*version).ok()?),
            }),
            [id, ..] => Some(Article {
                id: *id,
                version: None,
            }),
            [] => None,
        }
    }

    /// asks the articles API for the article's files.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the listing arrives with the figshare milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        Err(super::not_yet("figshare"))
    }
}

impl fmt::Display for Article {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "figshare:{}", self.id)?;
        if let Some(version) = self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

/// `FIGSHARE_TOKEN`, for private articles.
pub(super) fn credentials(var: Var<'_>) -> Vec<Credential> {
    let token = var("FIGSHARE_TOKEN")
        .and_then(|token| token.into_string().ok())
        .filter(|token| !token.trim().is_empty());
    token
        .map(|token| Credential {
            host: "api.figshare.com",
            header: "authorization",
            value: format!("token {}", token.trim()),
        })
        .into_iter()
        .collect()
}
