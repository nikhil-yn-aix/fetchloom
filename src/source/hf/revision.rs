//! what a revision points at: its commit, whether the repository is gated, and what its tags say.

use serde::Deserialize;

use super::{Repo, RepoKind, is_id};
use crate::error::{Error, Kind};
use crate::net::Api;

/// what the revision endpoint says about a repository.
#[derive(Deserialize)]
pub(super) struct Info {
    pub(super) id: String,
    pub(super) sha: String,
    /// `false`, or `auto` or `manual` for a repository whose terms must be accepted.
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default)]
    tags: Vec<String>,
    /// the front matter of the repository's card, asked for datasets only.
    #[serde(default, rename = "cardData")]
    card: Option<Card>,
}

/// the one part of a card fetchloom reads: the dataset's name for people.
#[derive(Deserialize)]
struct Card {
    #[serde(default)]
    pretty_name: serde_json::Value,
}

/// a card's name longer than this is a description, not a title.
const LONGEST_TITLE: usize = 100;

#[derive(Deserialize)]
struct Refusal {
    error: String,
}

impl Repo {
    /// the commit the revision points at, with what the hub says about the repository.
    pub(super) async fn revision(&self, api: &impl Api) -> Result<Info, Error> {
        let revision = self.revision.as_deref().unwrap_or("main");
        let mut url = self.api(&["revision", revision])?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("expand", "sha");
            // spaces have no terms to accept, and the hub refuses to expand what a kind lacks
            if self.kind != RepoKind::Space {
                query.append_pair("expand", "gated");
            }
            query.append_pair("expand", "tags");
            // a model's card has no dataset name, and can be large
            if self.kind == RepoKind::Dataset {
                query.append_pair("expand", "cardData");
            }
        }
        let response = api.get(&url).await?;
        let info: Info = crate::source::json(self, &response).map_err(|mut err| {
            if err.kind == Kind::AuthRequired {
                // the hub answers 401 alike for a private repository and for one that does not exist
                let kind = match self.kind {
                    RepoKind::Dataset => "dataset",
                    RepoKind::Model => "model",
                    RepoKind::Space => "space",
                };
                err.message = format!(
                    "huggingface.co has no {kind} {}, or it is private",
                    self.id
                );
                err.help(format!(
                    "check the name at {}. a private repository needs an account that can read it: sign in with `fetchloom auth login hf`",
                    self.page()
                ))
            } else {
                err
            }
        })?;
        if !is_commit(&info.sha) || !is_id(&info.id) {
            return Err(Error::new(
                Kind::SourceInvalid,
                format!(
                    "huggingface.co answered {self} with an id or commit fetchloom cannot read"
                ),
            )
            .detail(format!("id \"{}\", commit \"{}\"", info.id, info.sha))
            .help("report this response at https://github.com/nikhil-yn-aix/fetchloom/issues"));
        }
        if let Some(requested) = &self.revision
            && is_commit(requested)
            && !requested.eq_ignore_ascii_case(&info.sha)
        {
            return Err(Error::new(
                Kind::SourceInvalid,
                format!("huggingface.co answered {self} with a different commit"),
            )
            .detail(format!("expected {requested}, got {}", info.sha))
            .help(
                "run the same command again later. if it keeps happening, check the repository's commit history",
            ));
        }
        Ok(info)
    }

    /// stops at a gated repository whose terms the token on this machine has not accepted.
    pub(super) async fn check_access(&self, api: &impl Api) -> Result<(), Error> {
        let response = api.get(&self.api(&["auth-check"])?).await?;
        match response.status {
            200..=299 => Ok(()),
            401 | 403 => {
                let error = Error::new(
                    Kind::AuthRequired,
                    format!("{self} requires permission to download its files"),
                )
                .help(format!(
                    "request access and accept the terms at {} while signed in, then sign in here with `fetchloom auth login hf`",
                    self.page()
                ))
                .needs(crate::auth::Account::HuggingFace, Some(self.page()));
                Err(match response.json::<Refusal>() {
                    Ok(refusal) => error.detail(format!("huggingface.co says: {}", refusal.error)),
                    Err(_) => error,
                })
            }
            _ => crate::source::json::<serde::de::IgnoredAny>(self, &response).map(|_| ()),
        }
    }
}

impl Info {
    pub(super) fn is_gated(&self) -> bool {
        !matches!(
            self.gated,
            serde_json::Value::Null | serde_json::Value::Bool(false)
        )
    }

    /// the licenses the repository's card declares, which the hub copies into its tags.
    pub(super) fn license(&self) -> Option<String> {
        let licenses: Vec<&str> = self
            .tags
            .iter()
            .filter_map(|tag| tag.strip_prefix("license:"))
            .collect();
        (!licenses.is_empty()).then(|| licenses.join(", "))
    }

    /// the name the dataset's card gives it, such as `Wine Quality`, when it is one short line.
    pub(super) fn title(&self) -> Option<String> {
        let name = self.card.as_ref()?.pretty_name.as_str()?.trim();
        let fits = !name.is_empty()
            && !name.contains(char::is_control)
            && name.chars().count() <= LONGEST_TITLE;
        fits.then(|| name.to_owned())
    }

    /// the DOI the hub minted for the repository, when its owner asked for one.
    pub(super) fn doi(&self) -> Option<String> {
        self.tags
            .iter()
            .find_map(|tag| tag.strip_prefix("doi:"))
            .map(str::to_owned)
    }
}

/// a full git commit id: 40 hex digits.
pub(super) fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::recorded::fixture_bytes;

    #[test]
    fn the_doi_and_license_come_from_the_tags() {
        let info: Info =
            serde_json::from_slice(&fixture_bytes("hf/fineweb-edu-revision.json")).unwrap();
        assert_eq!(info.doi().as_deref(), Some("10.57967/hf/2497"));
        assert_eq!(info.license().as_deref(), Some("odc-by"));
    }

    #[test]
    fn the_title_is_the_name_the_card_gives() {
        let title = |card: &str| {
            let text = format!(
                r#"{{"id":"o/d","sha":"{}","cardData":{card}}}"#,
                "0".repeat(40)
            );
            serde_json::from_str::<Info>(&text).unwrap().title()
        };
        assert_eq!(
            title(r#"{"pretty_name":" Wine Quality ","license":"cc-by-4.0"}"#).as_deref(),
            Some("Wine Quality")
        );
        assert_eq!(title(r#"{"license":"cc0-1.0"}"#), None);
        assert_eq!(title("null"), None);
        assert_eq!(title(r#"{"pretty_name":["a","b"]}"#), None, "only a name");
        assert_eq!(title(r#"{"pretty_name":""}"#), None);
        assert_eq!(
            title(r#"{"pretty_name":"two\nlines"}"#),
            None,
            "a title is one line"
        );
        let paragraph = format!(r#"{{"pretty_name":"{}"}}"#, "word ".repeat(30));
        assert_eq!(title(&paragraph), None, "a description is no title");
        let info: Info =
            serde_json::from_slice(&fixture_bytes("hf/fineweb-edu-revision.json")).unwrap();
        assert_eq!(info.title(), None, "an answer without a card has no title");
    }
}
