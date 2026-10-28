//! hugging face: dataset, model and space repositories, listed at one commit through the tree
//! API and downloaded through `/resolve/<commit>/`.

use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Listing, Var, malformed};
use crate::error::Error;
use crate::net::{Api, Credential};

/// first path segments on huggingface.co that are pages, not model repositories.
const PAGES: [&str; 24] = [
    "api",
    "blog",
    "chat",
    "collections",
    "datasets",
    "docs",
    "enterprise",
    "huggingchat",
    "inference-endpoints",
    "join",
    "learn",
    "login",
    "models",
    "new",
    "notifications",
    "organizations",
    "papers",
    "posts",
    "pricing",
    "privacy",
    "search",
    "settings",
    "spaces",
    "tasks",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RepoKind {
    Dataset,
    Model,
    Space,
}

/// a repository, at a revision (a branch, a tag or a commit) or at its default branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repo {
    kind: RepoKind,
    /// `org/name`, or just `name` for the oldest repositories.
    id: String,
    revision: Option<String>,
}

impl Repo {
    /// reads the part after `hf:`, such as `datasets/org/name@main`. without a kind, it is a
    /// dataset.
    pub(super) fn parse(text: &str) -> Result<Repo, Error> {
        let bad = || {
            malformed(
                &format!("hf:{text}"),
                "hf:datasets/org/name, with @revision to pin one",
            )
        };
        let text = text.trim();
        let (path, revision) = match text.split_once('@') {
            Some((_, "")) => return Err(bad()),
            Some((path, revision)) => (path, Some(revision.to_owned())),
            None => (text, None),
        };
        let (kind, id) = match path.split_once('/') {
            Some(("datasets", id)) => (RepoKind::Dataset, id),
            Some(("models", id)) => (RepoKind::Model, id),
            Some(("spaces", id)) => (RepoKind::Space, id),
            _ => (RepoKind::Dataset, path),
        };
        if !is_id(id) {
            return Err(bad());
        }
        Ok(Repo {
            kind,
            id: id.to_owned(),
            revision,
        })
    }

    /// a pasted huggingface.co link, and the file in it when the link names one.
    pub(super) fn from_link(url: &Url) -> Option<(Repo, Option<String>)> {
        if !matches!(
            url.host_str()?,
            "huggingface.co" | "www.huggingface.co" | "hf.co"
        ) {
            return None;
        }
        let parts: Vec<String> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .map(|part| percent_decode_str(part).decode_utf8_lossy().into_owned())
            .collect();
        let (kind, rest) = match parts.first()?.as_str() {
            "datasets" => (RepoKind::Dataset, &parts[1..]),
            "spaces" => (RepoKind::Space, &parts[1..]),
            first if !PAGES.contains(&first) => (RepoKind::Model, &parts[..]),
            _ => return None,
        };
        let views = ["tree", "blob", "resolve", "commit", "discussions", "viewer"];
        let split = if rest.len() >= 2 && !views.contains(&rest[1].as_str()) {
            2
        } else {
            1
        };
        let id = rest.get(..split)?.join("/");
        if !is_id(&id) {
            return None;
        }
        let after = &rest[split..];
        let (revision, file) = match after.first().map(String::as_str) {
            Some("tree") => (after.get(1).cloned(), None),
            Some("blob" | "resolve") if after.len() > 2 => {
                (after.get(1).cloned(), Some(after[2..].join("/")))
            }
            _ => (None, None),
        };
        Some((Repo { kind, id, revision }, file))
    }

    /// lists the repository at one commit through the tree API.
    #[expect(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "the listing arrives with the hugging face milestone"
    )]
    pub(super) async fn resolve(&self, _api: &impl Api) -> Result<Listing, Error> {
        Err(super::not_yet("hugging face"))
    }
}

/// one or two parts of letters, digits, `-`, `_` and `.`.
fn is_id(text: &str) -> bool {
    let parts: Vec<&str> = text.split('/').collect();
    parts.len() <= 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.kind {
            RepoKind::Dataset => "datasets",
            RepoKind::Model => "models",
            RepoKind::Space => "spaces",
        };
        write!(f, "hf:{kind}/{}", self.id)?;
        if let Some(revision) = &self.revision {
            write!(f, "@{revision}")?;
        }
        Ok(())
    }
}

/// a hugging face token, from where the hugging face tools keep it.
pub(super) fn credentials(_var: Var<'_>) -> Vec<Credential> {
    Vec::new()
}
