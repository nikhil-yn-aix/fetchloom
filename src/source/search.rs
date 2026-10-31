//! dataset discovery across public catalogs, with no download or automatic selection.

use serde::{Deserialize, Serialize, Serializer};
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

/// asks all catalogs together, keeping rank order while giving each catalog a turn.
pub(crate) async fn search(api: &impl Api, words: &str) -> Result<Search, Error> {
    let (hf, kaggle, zenodo) = tokio::join!(
        hugging_face(api, words),
        kaggle(api, words),
        zenodo(api, words)
    );
    let mut result = Search::default();
    let mut first_error = None;
    let mut lists = Vec::new();
    for (source, answer) in [("hugging face", hf), ("kaggle", kaggle), ("zenodo", zenodo)] {
        match answer {
            Ok(candidates) => lists.push(candidates.into_iter()),
            Err(error) => {
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
    for _ in 0..3 {
        for list in &mut lists {
            result.candidates.extend(list.next());
        }
    }
    Ok(result)
}

/// words without a reference prefix are a search; bare DOIs still belong to the DOI resolver.
pub(crate) fn is_search(text: &str) -> bool {
    !text.contains(':')
        && text.chars().any(char::is_alphanumeric)
        && super::doi::Doi::bare(text.trim()).is_none()
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
                format!("a catalog returned an invalid dataset id: {error}"),
            )
        })
}

#[derive(Deserialize)]
struct HubMatch {
    id: String,
    downloads: Option<u64>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    gated: serde_json::Value,
}

async fn hugging_face(api: &impl Api, words: &str) -> Result<Vec<Candidate>, Error> {
    let url = query(
        "https://huggingface.co/api/datasets",
        &[
            ("search", words),
            ("limit", "3"),
            ("sort", "downloads"),
            ("direction", "-1"),
        ],
    )?;
    let response = api.get(&url).await?;
    let matches: Vec<HubMatch> = super::json(&url, &response)?;
    matches
        .into_iter()
        .filter(|item| !item.private)
        .take(3)
        .map(|item| {
            let gated = match item.gated {
                serde_json::Value::Bool(gated) => gated,
                serde_json::Value::String(mode) => !mode.is_empty() && mode != "false",
                _ => false,
            };
            Ok(Candidate {
                reference: reference(&format!("hf:datasets/{}", item.id))?,
                title: item.id,
                source: "hugging face",
                size: None,
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

async fn kaggle(api: &impl Api, words: &str) -> Result<Vec<Candidate>, Error> {
    let url = query(
        "https://api.kaggle.com/v1/datasets/list",
        &[("search", words), ("page", "1")],
    )?;
    let response = api.get(&url).await?;
    let matches: Vec<KaggleMatch> = super::json(&url, &response)?;
    matches
        .into_iter()
        .take(3)
        .map(|item| {
            Ok(Candidate {
                reference: reference(&format!("kaggle:{}", item.reference))?,
                title: item.title,
                source: "kaggle",
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

async fn zenodo(api: &impl Api, words: &str) -> Result<Vec<Candidate>, Error> {
    let words: String = words
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .collect();
    let url = query(
        "https://zenodo.org/api/records",
        &[
            ("q", &format!("title:({words})")),
            ("type", "dataset"),
            ("sort", "mostviewed"),
            ("size", "3"),
        ],
    )?;
    let response = api.get(&url).await?;
    let matches: ZenodoMatches = super::json(&url, &response)?;
    matches
        .hits
        .hits
        .into_iter()
        .take(3)
        .map(|item| {
            Ok(Candidate {
                reference: reference(&format!("zenodo:{}", item.id))?,
                title: item.metadata.title,
                source: "zenodo",
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

#[cfg(test)]
mod tests;
