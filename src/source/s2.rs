//! semantic scholar: papers of its academic graph by any id its API takes, pinned at the
//! open-access pdf it knows. its API answers one request at a time, a second apart with a key and
//! three without one, and is asked less often after it refuses one for load, as the network
//! paces any host whose source spaces its requests.

mod paper;

use std::sync::OnceLock;
use std::time::Duration;

use url::Url;

pub(crate) use self::paper::Paper;
use super::{Found, Reference, malformed};
use crate::auth::{Account, Origin, S2_HOST, S2_KEYS};
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

/// semantic scholar's API.
const API: &str = "https://api.semanticscholar.org";
/// what to write for a reference that is no paper of semantic scholar's.
const FORMS: &str = "s2:<paper id>, or an id from another index after its prefix: s2:DOI:<doi>, s2:ARXIV:<id>, s2:CorpusId:<number>, s2:PMID:<number>, s2:PMCID:<number>, s2:MAG:<number> or s2:ACL:<id>";

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
/// index after its prefix, such as `DOI:10.18653/v1/N18-3011`.
pub(super) fn parse(text: &str) -> Result<Reference, Error> {
    let text = text.trim();
    Paper::parse(text)
        .map(Reference::S2Paper)
        .ok_or_else(|| malformed(&format!("s2:{text}"), FORMS))
}

/// a pasted link to a paper's page on semanticscholar.org.
pub(super) fn from_link(url: &Url) -> Option<Reference> {
    match url.host_str()? {
        "www.semanticscholar.org" | "semanticscholar.org" | S2_HOST => {
            Paper::from_link(url).map(Reference::S2Paper)
        }
        _ => None,
    }
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
    let (key, help) = match origin {
        Origin::Variable(name) => (
            format!("the key in {name}"),
            format!(
                "the key may be old or wrong: set {name} to a current key from {S2_KEYS}, or unset {name} to use the key `fetchloom auth login s2` saves"
            ),
        ),
        origin => (
            format!("the key saved in {}", origin.shown()),
            "the key may be old or wrong: `fetchloom auth` shows where it comes from".to_owned(),
        ),
    };
    Error::new(
        Kind::AuthRequired,
        format!("{S2_HOST} refused {key} for {about}"),
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
