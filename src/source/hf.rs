//! hugging face: dataset, model and space repositories, listed at one commit through the tree
//! API and downloaded through `/resolve/<commit>/`.

mod listing;
mod revision;
mod tree;

use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

use super::malformed;
use crate::error::Error;

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

impl RepoKind {
    /// how the API and references name the kind.
    fn plural(self) -> &'static str {
        match self {
            RepoKind::Dataset => "datasets",
            RepoKind::Model => "models",
            RepoKind::Space => "spaces",
        }
    }
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

    /// the owner and the name of a dataset repository, such as `scikit-learn` and `iris`. none for
    /// a model, a space or an id of one part.
    pub(crate) fn dataset_parts(&self) -> Option<(&str, &str)> {
        (self.kind == RepoKind::Dataset)
            .then(|| self.id.split_once('/'))
            .flatten()
    }

    /// a pasted huggingface.co link, and the file or folder in it when the link names one.
    pub(super) fn from_link(url: &Url) -> Option<(Repo, Option<String>, Option<String>)> {
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
        let views = [
            "tree",
            "blob",
            "resolve",
            "raw",
            "blame",
            "commit",
            "commits",
            "discussions",
            "viewer",
        ];
        let view = |part: Option<&String>| part.is_some_and(|part| views.contains(&part.as_str()));
        // a page word second is a page of a one-part id, as in gpt2/tree/main, unless a page
        // word follows it too or nothing does, as for a repo named acme/tree
        let one_part = view(rest.get(1)) && rest.len() > 2 && !view(rest.get(2));
        let split = if rest.len() >= 2 && !one_part { 2 } else { 1 };
        let id = rest.get(..split)?.join("/");
        if !is_id(&id) {
            return None;
        }
        let after = &rest[split..];
        let inside = || (after.len() > 2).then(|| after[2..].join("/"));
        let (revision, file, folder) = match after.first().map(String::as_str) {
            Some("tree") => (after.get(1).cloned(), None, inside()),
            Some("commit") => (after.get(1).cloned(), None, None),
            Some("blob" | "resolve" | "raw" | "blame") if after.len() > 2 => {
                (after.get(1).cloned(), inside(), None)
            }
            _ => (None, None, None),
        };
        Some((Repo { kind, id, revision }, file, folder))
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
        write!(f, "hf:{}/{}", self.kind.plural(), self.id)?;
        if let Some(revision) = &self.revision {
            write!(f, "@{revision}")?;
        }
        Ok(())
    }
}
