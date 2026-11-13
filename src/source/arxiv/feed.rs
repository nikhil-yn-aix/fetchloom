//! what arXiv answers: its API's atom feed of papers, and the OAI-PMH record that states a
//! paper's license.

use std::fmt;

use url::Url;

use super::{API, Id, OAI};
use crate::error::{Error, Kind};
use crate::net::{self, Api};
use crate::source::xml;

/// a paper as the API's feed describes it, at the version it names.
pub(super) struct Entry {
    pub(super) id: Id,
    pub(super) version: u32,
    pub(super) title: String,
    pub(super) authors: Vec<String>,
    /// when its first version was submitted, as `2017-06-12T17:57:34Z`.
    pub(super) published: String,
}

impl Entry {
    /// its first author's last name and the year of its first version, as `Vaswani et al. 2017`.
    pub(super) fn byline(&self) -> Option<String> {
        let first = self.authors.first()?;
        let surname = first.rsplit(' ').next().unwrap_or(first);
        let who = if self.authors.len() > 1 {
            format!("{surname} et al.")
        } else {
            surname.to_owned()
        };
        let year = self
            .published
            .get(..4)
            .filter(|year| year.bytes().all(|byte| byte.is_ascii_digit()));
        Some(match year {
            Some(year) => format!("{who} {year}"),
            None => who,
        })
    }
}

/// one page of the feed: its papers, and how many the query matches in all.
pub(super) struct Page {
    pub(super) total: u64,
    pub(super) entries: Vec<Entry>,
}

/// asks arXiv's API with `pairs` for a page of papers about `about`, a reference or a search.
pub(super) async fn ask(
    api: &impl Api,
    pairs: &[(&str, &str)],
    about: &(dyn fmt::Display + Sync),
) -> Result<Page, Error> {
    let mut url = Url::parse(API).map_err(|err| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make arXiv's API link: {err}"),
        )
    })?;
    url.query_pairs_mut().extend_pairs(pairs.iter().copied());
    let response = api.get(&url).await?;
    if !(200..=299).contains(&response.status) {
        return Err(Error::new(
            Kind::NetworkStatus,
            format!(
                "{} answered {} for {about}",
                net::host(&response.url),
                response.status
            ),
        )
        .help("run the same command again later"));
    }
    page(&response.text()).map_err(|reason| unusable(about, &reason))
}

/// the papers a query of arXiv's API finds, the most relevant first.
pub(super) async fn search(api: &impl Api, query: &str, limit: usize) -> Result<Vec<Entry>, Error> {
    let most = limit.to_string();
    let pairs = [
        ("search_query", query),
        ("start", "0"),
        ("max_results", most.as_str()),
        ("sortBy", "relevance"),
    ];
    Ok(ask(api, &pairs, &format!("the search {query}"))
        .await?
        .entries)
}

/// the error for an answer from arXiv that fetchloom cannot read.
pub(super) fn unusable(about: &dyn fmt::Display, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!("arXiv sent an answer about {about} that fetchloom cannot use: {reason}"),
    )
    .help(crate::error::report_answer())
}

/// reads a page of the API's atom feed.
fn page(text: &str) -> Result<Page, String> {
    let feed = xml::parse(text)?;
    if feed.name != "feed" {
        return Err("it is not an atom feed".to_owned());
    }
    let total = feed
        .text_of("totalResults")
        .ok_or("it does not say how many papers match")?
        .trim()
        .parse()
        .map_err(|_| "its count of papers is not a number".to_owned())?;
    let entries = feed
        .children("entry")
        .map(entry)
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Page { total, entries })
}

fn entry(entry: &xml::Element) -> Result<Entry, String> {
    let link = entry.text_of("id").ok_or("a paper has no id")?.trim();
    if link.contains("/api/errors") {
        let said = entry.text_of("summary").unwrap_or(link).trim();
        return Err(format!("it said {}", said.escape_debug()));
    }
    let named = link
        .strip_prefix("http://arxiv.org/abs/")
        .or_else(|| link.strip_prefix("https://arxiv.org/abs/"))
        .ok_or_else(|| format!("\"{}\" is not a paper's page", link.escape_debug()))?;
    let (id, version) = Id::versioned(named)
        .ok_or_else(|| format!("\"{}\" is not an arXiv id", named.escape_debug()))?;
    let version = version.ok_or_else(|| format!("{id} has no version"))?;
    let title = words(entry.text_of("title").unwrap_or_default());
    if title.is_empty() {
        return Err(format!("{id} has no title"));
    }
    let authors = entry
        .children("author")
        .filter_map(|author| author.text_of("name"))
        .map(words)
        .filter(|name| !name.is_empty())
        .collect();
    Ok(Entry {
        id,
        version,
        title,
        authors,
        published: entry
            .text_of("published")
            .unwrap_or_default()
            .trim()
            .to_owned(),
    })
}

/// `text` with every run of spaces and line breaks one space, as a title wraps in the feed.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// what arXiv's OAI-PMH record of a paper says: its license and which versions have a pdf.
#[derive(Debug, Default)]
pub(super) struct Record {
    /// the license as the link arXiv writes, such as
    /// `http://arxiv.org/licenses/nonexclusive-distrib/1.0/`. none when it states none, as for a
    /// paper from before 2004, or has no record yet, as for one announced today.
    pub(super) license: Option<String>,
    /// for each version in turn, whether it was a withdrawal, which arXiv keeps no pdf of.
    withdrawn: Vec<bool>,
}

impl Record {
    /// whether `version` is a withdrawal, as far as the record says.
    pub(super) fn withdraws(&self, version: u32) -> bool {
        let at = usize::try_from(version).unwrap_or(usize::MAX);
        at.checked_sub(1)
            .and_then(|at| self.withdrawn.get(at))
            .copied()
            .unwrap_or(false)
    }
}

/// the record arXiv's OAI-PMH interface keeps of `id`, or an empty one when it has none yet.
pub(super) async fn record(api: &impl Api, id: &Id) -> Result<Record, Error> {
    let about = format!("arxiv:{id}");
    let mut url = Url::parse(OAI).map_err(|err| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make arXiv's OAI-PMH link: {err}"),
        )
    })?;
    url.query_pairs_mut()
        .append_pair("verb", "GetRecord")
        .append_pair("identifier", &format!("oai:arXiv.org:{id}"))
        .append_pair("metadataPrefix", "arXivRaw");
    let response = api.get(&url).await?;
    if !(200..=299).contains(&response.status) {
        return Err(Error::new(
            Kind::NetworkStatus,
            format!(
                "{} answered {} for the record of {about}",
                net::host(&response.url),
                response.status
            ),
        )
        .help("run the same command again later"));
    }
    read(&response.text(), id).map_err(|reason| unusable(&about, &reason))
}

/// the record an OAI-PMH answer holds for `id`.
fn read(text: &str, id: &Id) -> Result<Record, String> {
    let answer = xml::parse(text)?;
    if answer.name != "OAI-PMH" {
        return Err("it is not an OAI-PMH answer".to_owned());
    }
    // an id it has no record of yet comes back as an error, idDoesNotExist
    if answer.children("error").next().is_some() {
        return Ok(Record::default());
    }
    let paper = ["GetRecord", "record", "metadata", "arXivRaw"]
        .iter()
        .try_fold(&answer, |element, name| element.children(name).next())
        .ok_or("it holds no record")?;
    let named = paper.text_of("id").unwrap_or_default().trim();
    if named != id.to_string() {
        return Err(format!(
            "it holds the record of \"{}\"",
            named.escape_debug()
        ));
    }
    // a withdrawal is a version of no size whose source arXiv marks I, for ignore
    let withdrawn = paper
        .children("version")
        .map(|version| {
            version.text_of("size").map(str::trim) == Some("0kb")
                || version.text_of("source_type").map(str::trim) == Some("I")
        })
        .collect();
    Ok(Record {
        license: paper
            .text_of("license")
            .map(str::trim)
            .filter(|license| !license.is_empty())
            .map(str::to_owned),
        withdrawn,
    })
}
