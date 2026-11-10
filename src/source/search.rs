//! dataset discovery across public catalogs, with no download or automatic selection.

use std::pin::pin;
use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};
use tokio::time::Instant;
use url::Url;

use super::Reference;
use crate::error::{Error, Kind};
use crate::net::Api;

/// one public catalog match, named by a canonical reference rather than the search words.
#[derive(Debug, Serialize)]
pub(crate) struct Candidate {
    #[serde(serialize_with = "reference_text")]
    pub(crate) reference: Reference,
    pub(crate) title: String,
    pub(crate) source: &'static str,
    pub(crate) size: Option<u64>,
    pub(crate) downloads: Option<u64>,
    pub(crate) note: Option<String>,
}

fn reference_text<S: Serializer>(reference: &Reference, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(reference)
}

/// successful matches and any catalogs that could not answer.
#[derive(Debug, Default)]
pub(crate) struct Search {
    pub(crate) candidates: Vec<Candidate>,
    pub(crate) warnings: Vec<String>,
}

/// a catalog that answers keyword searches, named as references name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Catalog {
    Hf,
    Kaggle,
    Zenodo,
}

impl Catalog {
    pub(crate) const ALL: [Catalog; 3] = [Catalog::Hf, Catalog::Kaggle, Catalog::Zenodo];

    /// the catalog's name in the story and in `source`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Catalog::Hf => "hugging face",
            Catalog::Kaggle => "kaggle",
            Catalog::Zenodo => "zenodo",
        }
    }

    /// the catalog's name in `--source` and in references.
    fn name(self) -> &'static str {
        match self {
            Catalog::Hf => "hf",
            Catalog::Kaggle => "kaggle",
            Catalog::Zenodo => "zenodo",
        }
    }
}

/// the matches each catalog keeps when nobody asks for another number.
pub(crate) const LIMIT: usize = 3;

/// how long the other catalogs may take once one has answered, so one slow site cannot hold up
/// the matches the rest found. a healthy zenodo answered in 0.7 to 7 s; a failing one retried
/// for 76 s.
const GRACE: Duration = Duration::from_secs(10);

/// asks the `catalogs` together, keeping rank order while giving each catalog a turn, then moves
/// matches named exactly as the words to the front, in that same order.
pub(crate) async fn search(
    api: &impl Api,
    words: &str,
    catalogs: &[Catalog],
    limit: usize,
) -> Result<Search, Error> {
    let asked = |catalog: Catalog| catalogs.is_empty() || catalogs.contains(&catalog);
    let mut hf = pin!(async {
        if asked(Catalog::Hf) {
            Some(hugging_face(api, words, limit).await)
        } else {
            None
        }
    });
    let mut kaggle = pin!(async {
        if asked(Catalog::Kaggle) {
            Some(kaggle(api, words, limit).await)
        } else {
            None
        }
    });
    let mut zenodo = pin!(async {
        if asked(Catalog::Zenodo) {
            Some(zenodo(api, words, limit).await)
        } else {
            None
        }
    });
    let mut answers = [None, None, None];
    let mut late = None;
    while answers.iter().any(Option::is_none) {
        tokio::select! {
            answer = &mut hf, if answers[0].is_none() => answers[0] = Some(answer),
            answer = &mut kaggle, if answers[1].is_none() => answers[1] = Some(answer),
            answer = &mut zenodo, if answers[2].is_none() => answers[2] = Some(answer),
            () = tokio::time::sleep_until(late.unwrap_or_else(Instant::now)), if late.is_some() => {
                break;
            }
        }
        if late.is_none() && answers.iter().flatten().any(Option::is_some) {
            late = Some(Instant::now() + GRACE);
        }
    }
    let mut result = Search::default();
    let mut first_error = None;
    let mut lists = Vec::new();
    for (catalog, answer) in [Catalog::Hf, Catalog::Kaggle, Catalog::Zenodo]
        .into_iter()
        .zip(answers)
    {
        let source = catalog.label();
        match answer {
            None => result.warnings.push(format!(
                "{source} did not answer within {} s of the others; \
                 `fetchloom search \"{words}\" --source {}` waits for it",
                GRACE.as_secs(),
                catalog.name()
            )),
            Some(None) => {}
            Some(Some(Ok(candidates))) => lists.push(candidates.into_iter()),
            Some(Some(Err(error))) => {
                result
                    .warnings
                    .push(format!("{source} search failed: {error}"));
                first_error.get_or_insert(error);
            }
        }
    }
    if lists.is_empty()
        && let Some(error) = first_error
    {
        return Err(error);
    }
    for _ in 0..limit {
        for list in &mut lists {
            result.candidates.extend(list.next());
        }
    }
    let wanted = folded(words);
    result
        .candidates
        .sort_by_key(|candidate| !exact(candidate, &wanted));
    Ok(result)
}

/// text compared without case and with spaces, `-` and `_` alike.
fn folded(text: &str) -> String {
    text.to_lowercase()
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// whether a match's title or the last part of its id is the search words.
fn exact(candidate: &Candidate, wanted: &str) -> bool {
    let id = candidate.reference.to_string();
    let last = id.rsplit(['/', ':']).next().unwrap_or_default();
    folded(&candidate.title) == wanted || folded(last) == wanted
}

/// words without a reference prefix are a search; bare DOIs still belong to the DOI resolver, and
/// a path on this machine is never sent to a search site.
pub(crate) fn is_search(text: &str) -> bool {
    !text.contains(':')
        && text.chars().any(char::is_alphanumeric)
        && super::doi::Doi::bare(text.trim()).is_none()
        && !super::local::is_path(text)
}

fn query(base: &str, pairs: &[(&str, &str)]) -> Result<Url, Error> {
    let mut url = Url::parse(base).map_err(|error| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make a search link: {error}"),
        )
    })?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    Ok(url)
}

fn reference(text: &str) -> Result<Reference, Error> {
    Reference::parse(text)
        .map(|parsed| parsed.reference)
        .map_err(|error| {
            Error::new(
                Kind::SourceInvalid,
                format!("a search site returned an invalid dataset id: {error}"),
            )
        })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HubMatch {
    id: String,
    downloads: Option<u64>,
    main_size: Option<u64>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    gated: serde_json::Value,
}

async fn hugging_face(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let most = limit.to_string();
    let url = query(
        "https://huggingface.co/api/datasets",
        &[
            ("search", words),
            ("limit", &most),
            ("sort", "downloads"),
            ("direction", "-1"),
            // asking for any field returns only those asked for. mainSize is the size of the
            // files at the main revision, what fetchloom downloads; card data would add the
            // title some repositories set, but runs to hundreds of KB for popular words
            ("expand[]", "downloads"),
            ("expand[]", "mainSize"),
            ("expand[]", "gated"),
            ("expand[]", "private"),
        ],
    )?;
    let response = api.get(&url).await?;
    let matches: Vec<HubMatch> = super::json(&url, &response)?;
    matches
        .into_iter()
        .filter(|item| !item.private)
        .take(limit)
        .map(|item| {
            let gated = match item.gated {
                serde_json::Value::Bool(gated) => gated,
                serde_json::Value::String(mode) => !mode.is_empty() && mode != "false",
                _ => false,
            };
            Ok(Candidate {
                reference: reference(&format!("hf:datasets/{}", item.id))?,
                title: item.id,
                source: Catalog::Hf.label(),
                size: item.main_size,
                downloads: item.downloads,
                note: gated.then(|| "needs access".to_owned()),
            })
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KaggleMatch {
    #[serde(rename = "ref")]
    reference: String,
    title: String,
    total_bytes: Option<u64>,
    download_count: Option<u64>,
}

async fn kaggle(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let url = query(
        "https://api.kaggle.com/v1/datasets/list",
        &[("search", words), ("page", "1")],
    )?;
    let response = api.get(&url).await?;
    let matches: Vec<KaggleMatch> = super::json(&url, &response)?;
    matches
        .into_iter()
        .take(limit)
        .map(|item| {
            Ok(Candidate {
                reference: reference(&format!("kaggle:{}", item.reference))?,
                title: item.title,
                source: Catalog::Kaggle.label(),
                size: item.total_bytes,
                downloads: item.download_count,
                note: None,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct ZenodoMatches {
    hits: Hits,
}

#[derive(Deserialize)]
struct Hits {
    hits: Vec<ZenodoMatch>,
}

#[derive(Deserialize)]
struct ZenodoMatch {
    id: u64,
    metadata: Metadata,
    files: Option<Vec<File>>,
    stats: Option<Stats>,
}

#[derive(Deserialize)]
struct Metadata {
    title: String,
}

#[derive(Deserialize)]
struct File {
    size: Option<u64>,
}

#[derive(Deserialize)]
struct Stats {
    unique_downloads: Option<u64>,
}

async fn zenodo(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let words: String = words
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(c, '-' | '_'))
        .collect();
    // lowercase, so a word such as OR stays a word rather than an operator
    let words: Vec<String> = words.split_whitespace().map(str::to_lowercase).collect();
    let mut matches = zenodo_titles(api, &words.join(" AND "), limit).await?;
    if matches.is_empty() && words.len() > 1 {
        matches = zenodo_titles(api, &words.join(" OR "), limit).await?;
    }
    matches
        .into_iter()
        .take(limit)
        .map(|item| {
            Ok(Candidate {
                reference: reference(&format!("zenodo:{}", item.id))?,
                title: item.metadata.title,
                source: Catalog::Zenodo.label(),
                size: item.files.and_then(|files| {
                    files
                        .iter()
                        .try_fold(0_u64, |sum, file| sum.checked_add(file.size?))
                }),
                downloads: item.stats.and_then(|stats| stats.unique_downloads),
                note: None,
            })
        })
        .collect()
}

/// the most viewed datasets whose titles match `terms`.
async fn zenodo_titles(
    api: &impl Api,
    terms: &str,
    limit: usize,
) -> Result<Vec<ZenodoMatch>, Error> {
    let url = query(
        "https://zenodo.org/api/records",
        &[
            ("q", &format!("title:({terms})")),
            ("type", "dataset"),
            ("sort", "mostviewed"),
            ("size", &limit.to_string()),
        ],
    )?;
    let response = api.get(&url).await?;
    let matches: ZenodoMatches = super::json(&url, &response)?;
    Ok(matches.hits.hits)
}

#[cfg(test)]
mod tests;
