//! semantic scholar: papers of its academic graph by any id its API takes, pinned at the
//! open-access pdf it knows, and the datasets of its releases, whose files it links for a key
//! alone. its API answers one request at a time, a second apart with a key and three without one,
//! and is asked less often after it refuses one for load, as the network paces any host whose
//! source spaces its requests.

mod dataset;
mod paper;

use std::sync::OnceLock;
use std::time::Duration;

use url::Url;

pub(crate) use self::dataset::{Dataset, Relink};
pub(crate) use self::paper::Paper;
use super::{Found, Reference, malformed};
use crate::account::Account;
use crate::auth::{Origin, S2_HOST, S2_KEYS};
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

/// semantic scholar's API.
const API: &str = "https://api.semanticscholar.org";
/// what to write for a reference that is no paper or dataset of semantic scholar's.
const FORMS: &str = "s2:<paper id>, or an id from another index after its prefix: s2:DOI:<doi>, s2:ARXIV:<id>, s2:CorpusId:<number>, s2:PMID:<number>, s2:PMCID:<number>, s2:MAG:<number> or s2:ACL:<id>; or a dataset, as s2:papers, with @<release> to pin one, as s2:papers@2026-11-10";

/// what to give for a page of semantic scholar's that is no paper's, as its search page.
pub(super) const PAGES: &str = "paste a paper's page, as https://www.semanticscholar.org/paper/12b056fd90c1300c44c4a8cff9f01f0c7a75cab4, or write its id, as s2:DOI:10.1371/journal.pcbi.1003285";

/// how far apart requests paced as `name` start: semantic scholar's API takes one a second from a
/// key, its documented rate, and fetchloom asks three seconds apart without one, since every
/// program without a key shares one pool.
pub(super) fn spacing(name: &str) -> Option<Duration> {
    (name == S2_HOST).then(|| spaced(keyed()))
}

/// the spacing of requests to semantic scholar's API, with a key or without one.
fn spaced(keyed: bool) -> Duration {
    Duration::from_secs(if keyed { 1 } else { 3 })
}

/// how many requests paced as `name` may run at once: one for semantic scholar's API.
pub(super) fn most_at_once(name: &str) -> Option<usize> {
    (name == S2_HOST).then_some(1)
}

/// whether requests to semantic scholar's API carry a key, read once.
fn keyed() -> bool {
    static KEYED: OnceLock<bool> = OnceLock::new();
    *KEYED.get_or_init(|| sent_key().is_some())
}

/// where the key that goes to semantic scholar's API comes from, when one does.
fn sent_key() -> Option<Origin> {
    let account = Account::S2;
    account
        .find(&super::env)
        .filter(|found| account.sendable(found))
        .map(|found| found.origin)
}

/// reads the part after `s2:`: a paper, as the id semantic scholar gives it or one from another
/// index after its prefix, such as `DOI:10.18653/v1/N18-3011`, or a dataset, as `papers` or
/// `papers@2026-11-10`.
pub(super) fn parse(text: &str) -> Result<Reference, Error> {
    let text = text.trim();
    let bad = || malformed(&format!("s2:{text}"), FORMS);
    if let Some(paper) = Paper::parse(text) {
        return Ok(Reference::S2Paper(paper));
    }
    if text.contains(':') {
        return Err(match arxiv_version(text) {
            Some(paper) => bad().help(format!(
                "semantic scholar knows an arXiv paper by its id without a version, as s2:ARXIV:{}; add that version from arXiv as {paper}",
                paper.id()
            )),
            None => bad(),
        });
    }
    Dataset::parse(text)
        .map(Reference::S2Dataset)
        .ok_or_else(bad)
}

/// the arXiv paper at a version an `ARXIV:` id names with one, as `ARXIV:1706.03762v7`.
fn arxiv_version(text: &str) -> Option<super::arxiv::Paper> {
    let (prefix, rest) = text.split_once(':')?;
    if !prefix.trim().eq_ignore_ascii_case("ARXIV") {
        return None;
    }
    super::arxiv::Paper::at_version(rest.trim())
}

/// a pasted link to a paper's page on semanticscholar.org, or to a paper or a release's dataset
/// in its API, with the file a stable link of fetchloom's own names.
pub(super) fn from_link(url: &Url) -> Option<(Reference, Option<String>)> {
    match url.host_str()? {
        "www.semanticscholar.org" | "semanticscholar.org" => {
            Paper::from_link(url).map(|paper| (Reference::S2Paper(paper), None))
        }
        S2_HOST => Paper::from_link(url)
            .map(|paper| (Reference::S2Paper(paper), None))
            .or_else(|| {
                Dataset::from_link(url).map(|(dataset, file)| (Reference::S2Dataset(dataset), file))
            }),
        _ => None,
    }
}

/// the file of a dataset a stable link in data.lock names, whose signed link semantic scholar
/// gives only for a while: the request that gives a fresh one, and the file's name.
pub(crate) fn relink(url: &Url) -> Option<Relink> {
    Relink::of(url)
}

/// whether a signed link semantic scholar gave expires before `now`, or too soon after it.
pub(super) fn link_expired(link: &Url, now: std::time::SystemTime) -> bool {
    dataset::expired(link, now)
}

/// how a download from a signed link tells that the link expired, for a stable link of a
/// dataset's file.
pub(super) fn expiry_check(url: &Url) -> Option<crate::net::Expired> {
    Relink::of(url).map(|_| dataset::expired_answer as crate::net::Expired)
}

/// the papers semantic scholar's search finds for `words`, the most relevant first. a paper with
/// no open-access pdf says so, since adding it would be refused.
pub(crate) async fn search(
    api: &impl Api,
    words: &[String],
    limit: usize,
) -> Result<Vec<Found>, Error> {
    let query = words.join(" ");
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let mut url = Url::parse(&format!("{API}/graph/v1/paper/search")).map_err(|err| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make semantic scholar's search link: {err}"),
        )
    })?;
    url.query_pairs_mut()
        .append_pair("query", &query)
        .append_pair("limit", &limit.to_string())
        .append_pair("fields", paper::FIELDS);
    let response = api.get(&url).await.map_err(busy)?;
    let found: paper::Searched = answer(&format!("the search for {query}"), &response)?;
    Ok(found
        .data
        .into_iter()
        .flatten()
        .take(limit)
        .map(paper::Answer::found)
        .collect())
}

/// reads an answer of semantic scholar's API about `about`. a refusal names the key that went
/// with the request, or else the account that lifts it.
fn answer<T: serde::de::DeserializeOwned>(about: &str, response: &Response) -> Result<T, Error> {
    match (response.status, sent_key()) {
        (401 | 403, Some(origin)) => Err(refused_key(about, &origin)),
        (401 | 403, None) => Err(Error::new(
            Kind::AuthRequired,
            format!("semantic scholar refused access to {about}"),
        )
        .help(format!(
            "get a free key at {S2_KEYS}, then sign in with `fetchloom auth login s2`"
        ))
        .needs(Account::S2, None)),
        _ => super::json(&about, response),
    }
}

/// semantic scholar refused the key from `origin` as old or wrong. no sign-in is offered, since a
/// variable would win over the key it saves: the help says how to check or replace the key.
fn refused_key(about: &str, origin: &Origin) -> Error {
    let help = match origin {
        Origin::Variable(name) => format!(
            "the key may be old or wrong: set {name} to a current key from {S2_KEYS}, or unset {name} to use the key `fetchloom auth login s2` saves"
        ),
        _ => "the key may be old or wrong: `fetchloom auth` shows where it comes from".to_owned(),
    };
    Error::new(
        Kind::AuthRequired,
        format!("{S2_HOST} refused {} for {about}", origin.named()),
    )
    .help(help)
}

/// `error`, with what to do when semantic scholar's API is too busy to answer: wait, or, without
/// a key, get one, whose rate is its own.
fn busy(error: Error) -> Error {
    if error.kind != Kind::NetworkRateLimited {
        return error;
    }
    error.help(if keyed() {
        "semantic scholar is busy even at your key's rate of a request a second: run the same command again later".to_owned()
    } else {
        format!(
            "every program without a key shares one pool of semantic scholar's requests: run the same command again later, or get a free key at {S2_KEYS} and sign in with `fetchloom auth login s2`, for a rate of your own"
        )
    })
}

#[cfg(test)]
mod tests;
