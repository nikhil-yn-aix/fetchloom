//! The Hugging Face Hub: a dataset or model repository at a revision, its tree listed at the
//! commit the revision names, LFS files stated by their SHA-256.

use std::sync::Arc;

use fl_model::digest::{Algorithm, Digest};
use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_transfer::client::Client;
use fl_transfer::credentials::Variable;
use fl_transfer::fetch::Request;
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;

use crate::listing::{Entry, Listing, Meta};
use crate::paging::{self, Paging};
use crate::record;
use crate::source::{Read, Source, SourceError};
use crate::template::{escape, escape_segment};

const HUB: &str = "https://huggingface.co";

/// Datasets or models: where a repository's API and files live on the hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Datasets,
    Models,
}

impl Kind {
    const fn api(self) -> &'static str {
        match self {
            Self::Datasets => "datasets",
            Self::Models => "models",
        }
    }

    const fn files(self) -> &'static str {
        match self {
            Self::Datasets => "datasets/",
            Self::Models => "",
        }
    }
}

struct Repo<'a> {
    kind: Kind,
    id: &'a str,
    revision: &'a str,
}

#[derive(Deserialize)]
struct Revision {
    sha: String,
    #[serde(default)]
    gated: Value,
    #[serde(default, rename = "cardData")]
    card: Value,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: Option<u64>,
    lfs: Option<Lfs>,
}

#[derive(Deserialize)]
struct Lfs {
    oid: String,
}

/// Serves `hf:` references: `hf:datasets/org/name`, `hf:models/org/name` or `hf:org/name`
/// for a model, each with an optional `@revision`, a branch, tag or commit, `main` when left
/// out. Two requests and one more per thousand files: the revision, then the tree at its
/// commit.
pub struct HuggingFace {
    schemes: Vec<String>,
    base: SafeUrl,
    client: Arc<Client>,
}

impl HuggingFace {
    /// The adapter for huggingface.co.
    ///
    /// # Panics
    ///
    /// Never: the hub's address is a valid URL.
    #[must_use]
    pub fn new(client: Arc<Client>) -> Self {
        Self::at(
            SafeUrl::parse(HUB).unwrap_or_else(|_| unreachable!()),
            client,
        )
    }

    /// The adapter for a hub served at `base`, such as a mirror.
    #[must_use]
    pub fn at(base: SafeUrl, client: Arc<Client>) -> Self {
        Self {
            schemes: vec!["hf".to_owned()],
            base,
            client,
        }
    }

    fn url(&self, path: &str) -> Result<SafeUrl, SourceError> {
        self.base.join(path).map_err(|_| SourceError::Body {
            url: self.base.clone(),
            reason: format!("cannot be joined with `{path}`"),
        })
    }

    async fn listing(&self, reference: &Reference) -> Result<Listing, SourceError> {
        let raw = reference.as_str();
        let Target::Scheme { body, .. } = reference.target() else {
            return Err(SourceError::Unresolved {
                reference: raw.to_owned(),
            });
        };
        let repo = parse(body).ok_or_else(|| SourceError::Invalid {
            reference: raw.to_owned(),
            reason: "it is not datasets/org/name, models/org/name or org/name, with an optional @revision".to_owned(),
        })?;
        let mut revision = String::new();
        escape_segment(repo.revision, &mut revision);
        let api = format!("/api/{}/{}", repo.kind.api(), repo.id);
        let location = self.url(&format!("{api}/revision/{revision}"))?;
        let request = Request {
            url: &location,
            follow: true,
            typed: false,
        };
        let (page, value) = record::json(&self.client, &request, raw).await?;
        let found: Revision = serde_json::from_value(value).map_err(|error| SourceError::Body {
            url: page.url.clone(),
            reason: error.to_string(),
        })?;
        if found.sha.len() != 40 || !found.sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(SourceError::Record {
                path: "$.sha".to_owned(),
                url: page.url,
                problem: "a value that is not a commit",
            });
        }
        let gated = !matches!(found.gated, Value::Bool(false) | Value::Null);
        if gated && !page.credentialed {
            return Err(SourceError::Terms {
                reference: raw.to_owned(),
                page: format!("{HUB}/{}{}", repo.kind.files(), repo.id),
            });
        }
        let tree = self.url(&format!(
            "{api}/tree/{}?recursive=true&expand=false",
            found.sha
        ))?;
        let mut entries = Vec::new();
        paging::walk(
            &self.client,
            &tree,
            false,
            &Paging::Link,
            raw,
            |body, url| {
                let items: Vec<Item> =
                    serde_json::from_value(body.clone()).map_err(|error| SourceError::Body {
                        url: url.clone(),
                        reason: error.to_string(),
                    })?;
                let count = items.len();
                for item in items.into_iter().filter(|item| item.kind == "file") {
                    entries.push(self.entry(&repo, &found.sha, &item, url)?);
                }
                Ok(count)
            },
        )
        .await?;
        let resolved = format!("hf:{}/{}@{}", repo.kind.api(), repo.id, found.sha);
        Ok(Listing {
            resolved: resolved.parse().map_err(|_| SourceError::Invalid {
                reference: raw.to_owned(),
                reason: format!("it resolves to `{resolved}`, which is not a reference"),
            })?,
            meta: Meta {
                title: Some(repo.id.to_owned()),
                version: Some(found.sha.clone()),
                license: license(&found).map(|text| crate::license::spdx(&text)),
                doi: None,
                creators: Vec::new(),
            },
            entries,
        })
    }

    fn entry(
        &self,
        repo: &Repo<'_>,
        sha: &str,
        item: &Item,
        page: &SafeUrl,
    ) -> Result<Entry, SourceError> {
        let fail = |problem| SourceError::Record {
            path: "$[*].path".to_owned(),
            url: page.clone(),
            problem,
        };
        let path =
            DataPath::new(&item.path).map_err(|_| fail("a value that is not a valid path"))?;
        let mut escaped = String::new();
        escape(&item.path, &mut escaped);
        let url = self.url(&format!(
            "/{}{}/resolve/{sha}/{escaped}",
            repo.kind.files(),
            repo.id
        ))?;
        let stated = match &item.lfs {
            Some(lfs) => vec![
                Digest::from_hex(Algorithm::Sha256, &lfs.oid)
                    .map_err(|_| fail("an LFS oid that is not a SHA-256"))?,
            ],
            None => Vec::new(),
        };
        Ok(Entry {
            path,
            size: item.size,
            stated,
            urls: vec![url],
        })
    }
}

impl Source for HuggingFace {
    fn schemes(&self) -> &[String] {
        &self.schemes
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        Box::pin(self.listing(reference))
    }

    fn landing(&self, url: &str) -> Option<Reference> {
        let rest = url.strip_prefix(HUB)?.strip_prefix('/')?;
        let rest = rest.split(['?', '#']).next()?;
        let repo = parse(rest.trim_end_matches('/'))?;
        format!("hf:{}/{}", repo.kind.api(), repo.id).parse().ok()
    }

    fn credential(&self, read: Read) -> Option<Variable> {
        let host = self.base.origin()?.authority();
        Some(Variable::new(
            &host,
            "HF_TOKEN",
            "Authorization",
            "Bearer {token}",
            move |name| read(name),
        ))
    }
}

fn parse(body: &str) -> Option<Repo<'_>> {
    let (repo, revision) = body.split_once('@').unwrap_or((body, "main"));
    let (kind, id) = match repo.split_once('/') {
        Some(("datasets", id)) => (Kind::Datasets, id),
        Some(("models", id)) => (Kind::Models, id),
        _ => (Kind::Models, repo),
    };
    let word = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    let segments = id.split('/').count();
    let fits =
        (1..=2).contains(&segments) && id.split('/').all(word) && revision.split('/').all(word);
    fits.then_some(Repo { kind, id, revision })
}

fn license(found: &Revision) -> Option<String> {
    match found.card.get("license") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(items)) => items.first().and_then(Value::as_str).map(str::to_owned),
        _ => found
            .tags
            .iter()
            .find_map(|tag| tag.strip_prefix("license:"))
            .map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_a_kind_a_repository_and_a_revision() {
        let found = parse("datasets/rajpurkar/squad@v1.1").unwrap();
        assert_eq!(
            (found.kind, found.id, found.revision),
            (Kind::Datasets, "rajpurkar/squad", "v1.1")
        );
        let model = parse("gpt2").unwrap();
        assert_eq!(
            (model.kind, model.id, model.revision),
            (Kind::Models, "gpt2", "main")
        );
        assert_eq!(parse("models/a/b@refs/pr/1").unwrap().revision, "refs/pr/1");
        for body in ["", "a/b/c", "a/../b", "a b", "a/b@", "datasets/a/b@x//y"] {
            assert!(parse(body).is_none(), "{body}");
        }
    }
}
