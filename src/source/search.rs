//! dataset discovery across public catalogs, with no download or automatic selection.

use std::collections::HashMap;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;
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
    /// the matches found but not shown, since a search without a limit shows at most [`MOST`].
    pub(crate) left_out: usize,
}

/// a catalog that answers keyword searches, named as references name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Catalog {
    Hf,
    Kaggle,
    Zenodo,
    Ena,
    Geo,
    Dandi,
    #[value(name = "openneuro")]
    OpenNeuro,
}

impl Catalog {
    pub(crate) const ALL: [Catalog; 7] = [
        Catalog::Hf,
        Catalog::Kaggle,
        Catalog::Zenodo,
        Catalog::Ena,
        Catalog::Geo,
        Catalog::Dandi,
        Catalog::OpenNeuro,
    ];

    /// the catalog's name in the story and in `source`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Catalog::Hf => "hugging face",
            Catalog::Kaggle => "kaggle",
            Catalog::Zenodo => "zenodo",
            Catalog::Ena => "ENA",
            Catalog::Geo => "GEO",
            Catalog::Dandi => "dandi",
            Catalog::OpenNeuro => "openneuro",
        }
    }

    /// the catalog's name in `--source` and in references.
    fn name(self) -> &'static str {
        match self {
            Catalog::Hf => "hf",
            Catalog::Kaggle => "kaggle",
            Catalog::Zenodo => "zenodo",
            Catalog::Ena => "ena",
            Catalog::Geo => "geo",
            Catalog::Dandi => "dandi",
            Catalog::OpenNeuro => "openneuro",
        }
    }

    /// the catalog's first `limit` matches for `words`, in its own order.
    async fn find(
        self,
        api: &impl Api,
        words: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>, Error> {
        match self {
            Catalog::Hf => hugging_face(api, words, limit).await,
            Catalog::Kaggle => kaggle(api, words, limit).await,
            Catalog::Zenodo => zenodo(api, words, limit).await,
            Catalog::Ena => ena(api, words, limit).await,
            Catalog::Geo => geo(api, words, limit).await,
            Catalog::Dandi => dandi(api, words, limit).await,
            Catalog::OpenNeuro => openneuro(api, words, limit).await,
        }
    }
}

/// a catalog's search under way, which ends with the catalog's place among those asked.
type Asking<'a> = Pin<Box<dyn Future<Output = (usize, Result<Vec<Candidate>, Error>)> + Send + 'a>>;

/// the matches each catalog keeps when nobody asks for another number.
pub(crate) const LIMIT: usize = 3;

/// the most matches a search shows when nobody asks for a number, so a picker of them fits a
/// terminal 24 rows high.
pub(crate) const MOST: usize = 12;

/// how long the other catalogs may take once one answered, so one slow site cannot hold up the
/// rest: a healthy zenodo answered in 0.7 to 7 s, and a failing one retried for 76 s.
const GRACE: Duration = Duration::from_secs(10);

/// asks the `catalogs` together for `limit` matches each, taking turns in rank order, with exact
/// names moved to the front; with no `limit`, each gives [`LIMIT`] and [`MOST`] of them are shown.
pub(crate) async fn search(
    api: &impl Api,
    words: &str,
    catalogs: &[Catalog],
    limit: Option<usize>,
) -> Result<Search, Error> {
    let (limit, most) = limit.map_or((LIMIT, Some(MOST)), |limit| (limit, None));
    let asked: Vec<Catalog> = Catalog::ALL
        .into_iter()
        .filter(|catalog| catalogs.is_empty() || catalogs.contains(catalog))
        .collect();
    let mut asking: Vec<Asking<'_>> = asked
        .iter()
        .enumerate()
        .map(|(place, &catalog)| -> Asking<'_> {
            Box::pin(async move { (place, catalog.find(api, words, limit).await) })
        })
        .collect();
    let mut answers: Vec<Option<Result<Vec<Candidate>, Error>>> =
        asked.iter().map(|_| None).collect();
    let mut late = None;
    while !asking.is_empty() {
        tokio::select! {
            (place, answer) = poll_fn(|context| {
                for at in 0..asking.len() {
                    if let Poll::Ready(answered) = asking[at].as_mut().poll(context) {
                        drop(asking.swap_remove(at));
                        return Poll::Ready(answered);
                    }
                }
                Poll::Pending
            }) => {
                answers[place] = Some(answer);
                late.get_or_insert_with(|| Instant::now() + GRACE);
            }
            () = tokio::time::sleep_until(late.unwrap_or_else(Instant::now)), if late.is_some() => {
                break;
            }
        }
    }
    let mut result = Search::default();
    let mut first_error = None;
    let mut lists = Vec::new();
    for (catalog, answer) in asked.into_iter().zip(answers) {
        let source = catalog.label();
        match answer {
            None => result.warnings.push(format!(
                "{source} did not answer within {} s of the others; \
                 `fetchloom search \"{words}\" --source {}` waits for it",
                GRACE.as_secs(),
                catalog.name()
            )),
            Some(Ok(candidates)) => lists.push(candidates.into_iter()),
            Some(Err(error)) => {
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
    let mut ranked = Vec::new();
    for rank in 0..limit {
        for list in &mut lists {
            ranked.extend(list.next().map(|candidate| (rank, candidate)));
        }
    }
    let wanted = folded(words);
    if let Some(most) = most {
        result.left_out = keep(&mut ranked, most, |candidate| exact(candidate, &wanted));
    }
    result.candidates = ranked.into_iter().map(|(_, candidate)| candidate).collect();
    result
        .candidates
        .sort_by_key(|candidate| !exact(candidate, &wanted));
    Ok(result)
}

/// keeps `most` of the `ranked` matches in their order, each with its rank in its catalog: every
/// catalog's best, then those `named` as the words, then the rest by rank. how many it left out.
fn keep(
    ranked: &mut Vec<(usize, Candidate)>,
    most: usize,
    named: impl Fn(&Candidate) -> bool,
) -> usize {
    let found = ranked.len();
    let mut order: Vec<usize> = (0..found).collect();
    order.sort_by_key(|&at| {
        let (rank, candidate) = &ranked[at];
        (*rank > 0, !named(candidate))
    });
    let mut kept = vec![false; found];
    for at in order.into_iter().take(most) {
        kept[at] = true;
    }
    let mut at = 0;
    ranked.retain(|_| {
        at += 1;
        kept[at - 1]
    });
    found - ranked.len()
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
            // asking for any field returns only those asked; mainSize is what fetchloom downloads,
            // while card data, with titles some set, runs to hundreds of KB for popular words
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
            // no bytes is how kaggle lists a dataset it does not serve as files, which says
            // nothing of its size or its downloads
            let unserved = item.total_bytes == Some(0);
            Ok(Candidate {
                reference: reference(&format!("kaggle:{}", item.reference))?,
                title: item.title,
                source: Catalog::Kaggle.label(),
                size: item.total_bytes.filter(|_| !unserved),
                downloads: item.download_count.filter(|_| !unserved),
                note: None,
            })
        })
        .collect()
}

/// a public dataset one typo from a dataset its source would not show, as the source lists the
/// datasets of the same owner.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Near {
    /// the id that was given, such as `scikit-learn/irsi`.
    pub(crate) given: String,
    /// the id one typo from it, such as `scikit-learn/iris`.
    pub(crate) id: String,
    /// the reference that names the dataset, such as `hf:datasets/scikit-learn/iris`.
    pub(crate) reference: Reference,
}

/// a dataset an owner publishes, as hugging face lists one.
#[derive(Deserialize)]
struct Owned {
    id: String,
    #[serde(default)]
    private: bool,
}

/// a dataset an owner publishes, as kaggle lists one.
#[derive(Deserialize)]
struct Published {
    #[serde(rename = "ref")]
    reference: String,
}

/// the public dataset one typo from `reference`, a hugging face or kaggle dataset its source would
/// not show, among its owner's others, the most downloaded first on hugging face. it makes one
/// request, and a failed or rate-limited one finds none.
pub(crate) async fn near(api: &impl Api, reference: &Reference) -> Option<Near> {
    let (owner, name, prefix, url) = match reference {
        Reference::HuggingFace(repo) => {
            let (owner, name) = repo.dataset_parts()?;
            let url = query(
                "https://huggingface.co/api/datasets",
                &[
                    ("author", owner),
                    ("sort", "downloads"),
                    ("direction", "-1"),
                    ("limit", "100"),
                    ("expand[]", "private"),
                ],
            );
            (owner, name, "hf:datasets/", url)
        }
        Reference::Kaggle(dataset) => {
            let (owner, name) = dataset.parts();
            let url = query(
                "https://api.kaggle.com/v1/datasets/list",
                &[("user", owner), ("page", "1")],
            );
            (owner, name, "kaggle:", url)
        }
        _ => return None,
    };
    let url = url.ok()?;
    let response = api.get_once(&url).await.ok()?;
    let ids: Vec<String> = match reference {
        Reference::HuggingFace(_) => super::json::<Vec<Owned>>(&url, &response)
            .ok()?
            .into_iter()
            .filter(|owned| !owned.private)
            .map(|owned| owned.id)
            .collect(),
        _ => super::json::<Vec<Published>>(&url, &response)
            .ok()?
            .into_iter()
            .map(|published| published.reference)
            .collect(),
    };
    let id = ids.into_iter().find(|id| {
        id.split_once('/').is_some_and(|(by, other)| {
            by.eq_ignore_ascii_case(owner) && crate::name::one_typo(name, other)
        })
    })?;
    let reference = Reference::parse(&format!("{prefix}{id}")).ok()?.reference;
    Some(Near {
        given: format!("{owner}/{name}"),
        id,
        reference,
    })
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

/// the words a catalog with a query language reads as words alone: letters, digits, `-` and `_`,
/// lowercase, so a word such as OR stays a word rather than an operator.
fn plain_words(words: &str) -> Vec<String> {
    let words: String = words
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || matches!(c, '-' | '_'))
        .collect();
    words.split_whitespace().map(str::to_lowercase).collect()
}

async fn zenodo(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let words = plain_words(words);
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

/// ENA's studies, as EBI's search lists them: by accession, with a title or a description.
#[derive(Deserialize)]
struct Studies {
    entries: Vec<Study>,
}

#[derive(Deserialize)]
struct Study {
    acc: String,
    #[serde(default)]
    fields: StudyFields,
}

#[derive(Default, Deserialize)]
struct StudyFields {
    #[serde(default)]
    name: Vec<String>,
    #[serde(default)]
    description: Vec<String>,
}

/// the studies EBI's search finds in ENA, the most relevant first. a study names every run in
/// it, and EBI's search gives no size or downloads.
async fn ena(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let words = plain_words(words);
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let url = query(
        "https://www.ebi.ac.uk/ebisearch/ws/rest/sra-study",
        &[
            ("query", &words.join(" ")),
            ("size", &limit.to_string()),
            ("format", "json"),
            ("fields", "name,description"),
        ],
    )?;
    let response = api.get(&url).await?;
    let studies: Studies = super::json(&url, &response)?;
    studies
        .entries
        .into_iter()
        .take(limit)
        .map(|study| {
            let fields = study.fields;
            let title = fields
                .name
                .into_iter()
                .chain(fields.description)
                .find(|text| !text.trim().is_empty())
                .unwrap_or_else(|| study.acc.clone());
            Ok(Candidate {
                reference: reference(&format!("ena:{}", study.acc))?,
                title,
                source: Catalog::Ena.label(),
                size: None,
                downloads: None,
                note: None,
            })
        })
        .collect()
}

/// NCBI's E-utilities, which search GEO and summarize what they find.
const EUTILS: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";

#[derive(Deserialize)]
struct Found {
    esearchresult: FoundIds,
}

#[derive(Deserialize)]
struct FoundIds {
    idlist: Vec<String>,
}

#[derive(Deserialize)]
struct Summaries {
    result: Summarized,
}

/// each summary by the id the search found, beside the list of those ids.
#[derive(Deserialize)]
struct Summarized {
    #[serde(default, rename = "uids")]
    _ids: serde::de::IgnoredAny,
    #[serde(flatten)]
    series: HashMap<String, Series>,
}

#[derive(Deserialize)]
struct Series {
    accession: String,
    title: String,
}

/// the GEO series NCBI's search finds, in its order, then their titles from a second request.
/// GEO gives no size or downloads.
async fn geo(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let words = plain_words(words);
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let url = query(
        &format!("{EUTILS}/esearch.fcgi"),
        &[
            ("db", "gds"),
            ("term", &format!("{} AND gse[ETYP]", words.join(" "))),
            ("retmax", &limit.to_string()),
            ("retmode", "json"),
            ("tool", "fetchloom"),
        ],
    )?;
    let response = api.get(&url).await?;
    let found: Found = super::json(&url, &response)?;
    let ids: Vec<String> = found.esearchresult.idlist.into_iter().take(limit).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let url = query(
        &format!("{EUTILS}/esummary.fcgi"),
        &[
            ("db", "gds"),
            ("id", &ids.join(",")),
            ("retmode", "json"),
            ("tool", "fetchloom"),
        ],
    )?;
    let response = api.get(&url).await?;
    let mut summaries: Summaries = super::json(&url, &response)?;
    ids.iter()
        .filter_map(|id| summaries.result.series.remove(id))
        .map(|series| {
            Ok(Candidate {
                reference: reference(&format!("geo:{}", series.accession))?,
                title: series.title,
                source: Catalog::Geo.label(),
                size: None,
                downloads: None,
                note: None,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct Dandisets {
    results: Vec<Dandiset>,
}

#[derive(Deserialize)]
struct Dandiset {
    identifier: String,
    embargo_status: Option<String>,
    most_recent_published_version: Option<DandiVersion>,
    draft_version: Option<DandiVersion>,
}

#[derive(Deserialize)]
struct DandiVersion {
    name: String,
    size: Option<u64>,
    asset_count: Option<u64>,
}

/// the most starred dandisets whose text holds the words, as their newest version or draft
/// describes them, but for embargoed ones, which fetchloom cannot download, and empty ones.
async fn dandi(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let url = query(
        "https://api.dandiarchive.org/api/dandisets/",
        &[
            ("search", words.trim()),
            ("page_size", &limit.to_string()),
            ("ordering", "-stars"),
        ],
    )?;
    let response = api.get(&url).await?;
    let found: Dandisets = super::json(&url, &response)?;
    found
        .results
        .into_iter()
        .filter(|dandiset| {
            dandiset
                .embargo_status
                .as_deref()
                .is_none_or(|status| status == "OPEN")
        })
        .filter_map(|dandiset| {
            let version = dandiset
                .most_recent_published_version
                .or(dandiset.draft_version)?;
            (version.asset_count != Some(0)).then_some((dandiset.identifier, version))
        })
        .take(limit)
        .map(|(identifier, version)| {
            Ok(Candidate {
                reference: reference(&format!("dandi:{identifier}"))?,
                title: version.name,
                source: Catalog::Dandi.label(),
                size: version.size,
                downloads: None,
                note: None,
            })
        })
        .collect()
}

/// what openneuro's search asks of each dataset it finds.
const DATASETS: &str = "query($words:[String!],$first:Int){advancedSearch(query:{keywords:$words},first:$first){edges{node{id latestSnapshot{size description{Name}} analytics{downloads}}}}}";

/// the words and the count of a search, sent apart from the query so no word can change it.
#[derive(Serialize)]
struct Variables<'a> {
    words: [&'a str; 1],
    first: usize,
}

#[derive(Deserialize)]
struct Searched {
    data: Option<SearchedData>,
    #[serde(default)]
    errors: Vec<Problem>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchedData {
    advanced_search: Option<Datasets>,
}

#[derive(Deserialize)]
struct Datasets {
    edges: Vec<Option<Edge>>,
}

#[derive(Deserialize)]
struct Edge {
    node: Option<Node>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Node {
    id: String,
    latest_snapshot: Option<Snapshot>,
    analytics: Option<Analytics>,
}

#[derive(Deserialize)]
struct Snapshot {
    size: Option<u64>,
    description: Option<Description>,
}

#[derive(Deserialize)]
struct Description {
    #[serde(rename = "Name")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct Analytics {
    downloads: Option<u64>,
}

#[derive(Deserialize)]
struct Problem {
    message: String,
}

/// the datasets openneuro's search ranks first, at their newest snapshot, but for private ones,
/// answered with an error in their place, and ones with no snapshot to download.
async fn openneuro(api: &impl Api, words: &str, limit: usize) -> Result<Vec<Candidate>, Error> {
    let variables = serde_json::to_string(&Variables {
        words: [words.trim()],
        first: limit,
    })
    .map_err(|error| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make a search link: {error}"),
        )
    })?;
    let url = query(
        super::openneuro::API,
        &[("query", DATASETS), ("variables", &variables)],
    )?;
    let response = api.get_with(&url, super::openneuro::PREFLIGHT).await?;
    let searched: Searched = super::json(&url, &response)?;
    let Some(datasets) = searched.data.and_then(|data| data.advanced_search) else {
        let said = match searched.errors.first() {
            Some(problem) => format!(
                "an error: {}",
                problem.message.lines().next().unwrap_or_default()
            ),
            None => "no datasets".to_owned(),
        };
        return Err(Error::new(
            Kind::SourceInvalid,
            format!("openneuro.org answered the search with {said}"),
        )
        .help(crate::error::report_answer()));
    };
    datasets
        .edges
        .into_iter()
        .filter_map(|edge| {
            let node = edge?.node?;
            Some((node.id, node.latest_snapshot?, node.analytics))
        })
        .take(limit)
        .map(|(id, snapshot, analytics)| {
            let title = snapshot
                .description
                .and_then(|description| description.name)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| id.clone());
            Ok(Candidate {
                reference: reference(&format!("openneuro:{id}"))?,
                title,
                source: Catalog::OpenNeuro.label(),
                size: snapshot.size,
                downloads: analytics.and_then(|analytics| analytics.downloads),
                note: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
