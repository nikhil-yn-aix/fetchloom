//! what a revision points at: its commit, whether the repository is gated, and what its tags say.

use serde::Deserialize;

use super::{Repo, RepoKind, is_id};
use crate::auth::{Account, Origin};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::Access;

/// what the revision endpoint says about a repository.
#[derive(Deserialize)]
pub(super) struct Info {
    pub(super) id: String,
    pub(super) sha: String,
    /// `false`, or `auto` or `manual` for a repository whose terms must be accepted.
    #[serde(default)]
    gated: serde_json::Value,
    /// whether only accounts the owner allows may read the repository.
    #[serde(default)]
    private: Option<bool>,
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
            query.append_pair("expand", "private");
            query.append_pair("expand", "tags");
            // a model's card has no dataset name, and can be large
            if self.kind == RepoKind::Dataset {
                query.append_pair("expand", "cardData");
            }
        }
        let response = api.get(&url).await?;
        let info: Info = match crate::source::json(self, &response) {
            Ok(info) => info,
            Err(err) if err.kind == Kind::AuthRequired => return Err(self.unseen(api, err).await),
            Err(err) => return Err(err),
        };
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

    /// the error for a repository the hub refused to show, which it does alike for a private one
    /// and for one that does not exist, as after a typo. a link without `/datasets/` names a
    /// model, so a model the hub does not show, whose name a dataset has, points to the dataset.
    async fn unseen(&self, api: &impl Api, refused: Error) -> Error {
        if let Some(dataset) = self.same_named_dataset(api).await {
            return Error::new(
                Kind::ReferenceUnresolved,
                format!(
                    "huggingface.co has no public model {}, but it has a dataset of that name",
                    self.id
                ),
            )
            .help(format!("for the dataset, use {}", dataset.page()));
        }
        let kind = match self.kind {
            RepoKind::Dataset => "dataset",
            RepoKind::Model => "model",
            RepoKind::Space => "space",
        };
        let error = Error::new(
            refused.kind,
            format!("huggingface.co has no {kind} {}, or it is private", self.id),
        );
        let page = self.page();
        match sent_key() {
            // a sign-in saves a key the variable would still win over
            Some(Origin::Variable(name)) => error
                .detail(format!("the key in {name} went with the request"))
                .help(format!(
                    "check the name at {page}. a private repository needs a key that can read it: if the key in {name} is old or wrong, set {name} to a current one from {TOKENS}"
                )),
            Some(origin) => error
                .detail(format!("{} went with the request", key(&origin)))
                .help(format!(
                    "check the name at {page}. a private repository needs a key that can read it: sign in again with `fetchloom auth login hf`"
                ))
                .needs_or_missing(Account::HuggingFace),
            None => error
                .help(format!(
                    "check the name at {page}. a private repository needs an account that can read it: sign in with `fetchloom auth login hf`"
                ))
                .needs_or_missing(Account::HuggingFace),
        }
    }

    /// the dataset with this model's name, when the hub shows one.
    async fn same_named_dataset(&self, api: &impl Api) -> Option<Repo> {
        if self.kind != RepoKind::Model {
            return None;
        }
        let dataset = Repo {
            kind: RepoKind::Dataset,
            id: self.id.clone(),
            revision: None,
        };
        let response = api.get(&dataset.api(&[]).ok()?).await.ok()?;
        (response.status == 200).then_some(dataset)
    }

    /// stops at a gated repository whose terms the token on this machine has not accepted.
    pub(super) async fn check_access(&self, api: &impl Api) -> Result<(), Error> {
        let response = api.get(&self.api(&["auth-check"])?).await?;
        match response.status {
            200..=299 => Ok(()),
            status @ (401 | 403) => {
                // a 401 says the hub took the request as from nobody, so a key that went with it
                // was refused
                let error = match sent_key().filter(|_| status == 401) {
                    Some(origin) => self.refused_key(&origin),
                    None => Error::new(
                        Kind::AuthRequired,
                        format!("{self} requires permission to download its files"),
                    )
                    .help(format!(
                        "request access and accept the terms at {} while signed in, then sign in here with `fetchloom auth login hf`",
                        self.page()
                    ))
                    .needs(Account::HuggingFace, Some(self.page())),
                };
                Err(match response.json::<Refusal>() {
                    Ok(refusal) => error.detail(format!("huggingface.co says: {}", refusal.error)),
                    Err(_) => error,
                })
            }
            _ => crate::source::json::<serde::de::IgnoredAny>(self, &response).map(|_| ()),
        }
    }

    /// the hub refused the key from `origin`, which went with the request: it is old or wrong.
    /// no sign-in is offered, as the help says what replaces the key, and a key from a variable
    /// would win over one a sign-in saves.
    fn refused_key(&self, origin: &Origin) -> Error {
        let help = match origin {
            Origin::Variable(name) => format!(
                "the key may be old or wrong: set {name} to a current key from {TOKENS}, or unset {name} to use the key `fetchloom auth login hf` saves"
            ),
            _ => "the key may be old or wrong: sign in again with `fetchloom auth login hf`"
                .to_owned(),
        };
        Error::new(
            Kind::AuthRequired,
            format!("huggingface.co refused {} for {self}", key(origin)),
        )
        .help(help)
    }
}

impl Info {
    pub(super) fn is_gated(&self) -> bool {
        !matches!(
            self.gated,
            serde_json::Value::Null | serde_json::Value::Bool(false)
        )
    }

    /// who may read the repository, as the hub says: accepted terms for a gated one, an account
    /// for a private one, and anyone only when it says the repository is not private.
    pub(super) fn access(&self) -> Access {
        match self.private {
            _ if self.is_gated() => Access::Terms,
            Some(true) => Access::Login,
            Some(false) => Access::Public,
            None => Access::Unknown,
        }
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

/// where hugging face keys are made.
const TOKENS: &str = "https://huggingface.co/settings/tokens";

/// where the key requests to huggingface.co carry comes from, as the network that sends them
/// found it, when there is one a request can carry.
fn sent_key() -> Option<Origin> {
    let account = Account::HuggingFace;
    account
        .find(&crate::source::env)
        .filter(|found| account.sendable(found))
        .map(|found| found.origin)
}

/// a key as messages name it, by where it comes from and never by its value.
fn key(origin: &Origin) -> String {
    match origin {
        Origin::Variable(name) => format!("the key in {name}"),
        origin => format!("the key saved in {origin}"),
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

    /// a refused key is named by where it comes from, never by its value: a variable, whose
    /// replacement the help names, or the file a sign-in saved it to, which signing in again
    /// replaces. no sign-in is offered for either.
    #[test]
    fn a_refused_key_is_named_by_where_it_comes_from() {
        let repo = Repo::parse("datasets/ILSVRC/imagenet-1k").unwrap();
        let variable = repo.refused_key(&Origin::Variable("HF_TOKEN"));
        assert_eq!(variable.kind, Kind::AuthRequired);
        assert_eq!(
            variable.message,
            "huggingface.co refused the key in HF_TOKEN for hf:datasets/ILSVRC/imagenet-1k"
        );
        assert_eq!(
            variable.help.as_deref(),
            Some(
                "the key may be old or wrong: set HF_TOKEN to a current key from https://huggingface.co/settings/tokens, or unset HF_TOKEN to use the key `fetchloom auth login hf` saves"
            )
        );
        assert!(variable.need.is_none());
        let token = std::path::PathBuf::from("/home/ada/.cache/huggingface/token");
        let saved = repo.refused_key(&Origin::File(token));
        assert_eq!(
            saved.message,
            "huggingface.co refused the key saved in /home/ada/.cache/huggingface/token for hf:datasets/ILSVRC/imagenet-1k"
        );
        assert_eq!(
            saved.help.as_deref(),
            Some("the key may be old or wrong: sign in again with `fetchloom auth login hf`")
        );
        assert!(saved.need.is_none());
    }

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
