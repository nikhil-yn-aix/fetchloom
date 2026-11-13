//! arXiv: a paper by its id, at a version or its newest, and the papers of a month or of one
//! category in a month. pdfs come from cornell's public mirror on google cloud storage where it
//! holds them, else from export.arxiv.org, the host arXiv keeps for programs. requests to arXiv's
//! own hosts go one at a time and three seconds apart, as arXiv's terms ask.

mod feed;
mod month;
mod paper;

use std::fmt;
use std::time::Duration;

use url::Url;

pub(crate) use self::month::Month;
use super::{Found, Reference, Remote, malformed};
use crate::error::{Error, Kind};
use crate::net::Api;

/// arXiv's API, which describes papers and finds them.
const API: &str = "https://export.arxiv.org/api/query";
/// where arXiv sends programs for a paper's pdf.
const PDFS: &str = "https://export.arxiv.org/pdf/";
/// arXiv's OAI-PMH interface, the one place a program can read a paper's license.
const OAI: &str = "https://oaipmh.arxiv.org/oai";
/// cornell's public mirror of arXiv's pdfs on google cloud storage, which anyone may read for
/// free, every version of a paper in the folder of the month its id was given.
const MIRROR: &str = "arxiv-dataset";

/// arXiv's terms ask a program for one request every three seconds, on one connection, across
/// its API, its OAI-PMH interface and its pages alike.
const SPACING: Duration = Duration::from_secs(3);
/// the name requests to every arXiv host are paced under, since its terms limit them together.
const PACED_AS: &str = "arxiv.org";

/// what to write for a reference that is no arXiv id, month or category.
const FORMS: &str = "arxiv:1706.03762, with @v7 to pin a version, arxiv:2401 for a month's papers or arxiv:cs.LG/2401 for one category's";

/// the name a request to `host` is paced under, when it is one of arXiv's.
pub(super) fn paced_as(host: &str) -> Option<&'static str> {
    (host == PACED_AS || host.ends_with(".arxiv.org")).then_some(PACED_AS)
}

/// how far apart requests paced as `name` start: arXiv's, three seconds.
pub(super) fn spacing(name: &str) -> Option<Duration> {
    (name == PACED_AS).then_some(SPACING)
}

/// how many requests paced as `name` may run at once: one for arXiv's.
pub(super) fn most_at_once(name: &str) -> Option<usize> {
    (name == PACED_AS).then_some(1)
}

/// reads the part after `arxiv:`: a paper, `1706.03762` or `hep-th/9901001`, with `v7` or `@v7`
/// for a version; a month, `2401`; or a category in a month, `cs.LG/2401`.
pub(super) fn parse(text: &str) -> Result<Reference, Error> {
    let text = text.trim();
    let bad = || malformed(&format!("arxiv:{text}"), FORMS);
    let (head, version) = match text.split_once('@') {
        Some((head, version)) => {
            let version = version.strip_prefix(['v', 'V']).unwrap_or(version);
            (head, Some(number(version).ok_or_else(bad)?))
        }
        None => (text, None),
    };
    let last = head.rsplit('/').next().unwrap_or(head);
    if last.len() == 4 && last.bytes().all(|byte| byte.is_ascii_digit()) {
        if version.is_some() {
            return Err(bad().help(
                "a month's papers have no version of their own: write the month alone, as arxiv:2401, and each paper comes at its newest version",
            ));
        }
        return Month::parse(head).map(Reference::ArxivMonth);
    }
    let (id, written) = Id::versioned(head).ok_or_else(bad)?;
    if version.is_some() && written.is_some() {
        return Err(bad());
    }
    Ok(Reference::Arxiv(Paper {
        id,
        version: version.or(written),
    }))
}

/// a pasted link to a paper's page or pdf on arXiv: `/abs/`, `/pdf/` or `/html/`, with or
/// without a version.
pub(super) fn from_link(url: &Url) -> Option<Paper> {
    if !matches!(
        url.host_str()?,
        "arxiv.org" | "www.arxiv.org" | "export.arxiv.org"
    ) {
        return None;
    }
    let path = url.path().trim_start_matches('/');
    let (page, rest) = path.split_once('/')?;
    if !matches!(page, "abs" | "pdf" | "html") {
        return None;
    }
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".pdf").unwrap_or(rest);
    let (id, version) = Id::versioned(rest)?;
    Some(Paper { id, version })
}

/// a whole number of at least one, as a version: `7` of `v7`.
fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|number| *number > 0)
}

/// the mirror's objects under `key`, a folder ending in `/` or one object's whole key, each named
/// by its key below the folder. none when the mirror holds nothing there.
async fn mirrored(api: &impl Api, key: &str) -> Result<Vec<Remote>, Error> {
    let prefix =
        super::bucket::Prefix::parse(super::bucket::Store::Gcs, &format!("//{MIRROR}/{key}"))?;
    match prefix.resolve(api).await {
        Ok(listing) => Ok(listing.files),
        Err(error) if error.kind == Kind::ReferenceUnresolved => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// a paper's id without its version, as arXiv writes it: `1706.03762`, or before april 2007 an
/// archive and a number, `hep-th/9901001`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Id(String);

impl Id {
    /// reads an id and the version written after it, as `1706.03762v7`. an old id may hold a
    /// subject class, as `math.GT/0309136`, which arXiv's API knows only as `math/0309136`.
    fn versioned(text: &str) -> Option<(Id, Option<u32>)> {
        let (id, version) = match text.rfind(['v', 'V']) {
            Some(at) if at > 0 && number(&text[at + 1..]).is_some() => {
                (&text[..at], number(&text[at + 1..]))
            }
            _ => (text, None),
        };
        Some((Id::parse(id)?, version))
    }

    fn parse(text: &str) -> Option<Id> {
        let digits = |text: &str, count: usize| {
            text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit())
        };
        if let Some((archive, number)) = text.split_once('/') {
            let archive = match archive.split_once('.') {
                Some((archive, subject))
                    if subject.len() == 2 && subject.bytes().all(|b| b.is_ascii_alphabetic()) =>
                {
                    archive
                }
                Some(_) => return None,
                None => archive,
            };
            let archive = archive.to_ascii_lowercase();
            let valid = is_archive(&archive)
                && digits(number, 7)
                && Month::year(&number[..4])
                    .is_some_and(|year| year < 2007 || &number[..4] <= "0703");
            return valid.then(|| Id(format!("{archive}/{number}")));
        }
        let (month, number) = text.split_once('.')?;
        let width = if month <= "1412" { 4 } else { 5 };
        let valid = digits(month, 4) && month >= "0704" && digits(number, width);
        (valid && Month::year(month).is_some()).then(|| Id(text.to_owned()))
    }

    /// the month arXiv gave the id: `1706`, or `9901` for `hep-th/9901001`.
    fn month(&self) -> &str {
        match self.0.split_once('/') {
            Some((_, number)) => &number[..4],
            None => &self.0[..4],
        }
    }

    /// the mirror's folder for the id's archive and month: `arxiv/arxiv/pdf/1706/`, or
    /// `arxiv/hep-th/pdf/9901/` for an old id.
    fn folder(&self) -> String {
        let archive = self
            .0
            .split_once('/')
            .map_or("arxiv", |(archive, _)| archive);
        format!("arxiv/{archive}/pdf/{}/", self.month())
    }

    /// the name the mirror gives a version's pdf in [`Id::folder`]: `1706.03762v7.pdf`, or
    /// `9901001v1.pdf` for `hep-th/9901001`.
    fn mirrored(&self, version: u32) -> String {
        let number = self.0.rsplit('/').next().unwrap_or(&self.0);
        format!("{number}v{version}.pdf")
    }

    /// the name a version's pdf takes in a dataset: `1706.03762v7.pdf`, as arXiv names it, and
    /// `hep-th9901001v1.pdf` for an old id, so two archives' papers never share a name.
    fn file(&self, version: u32) -> String {
        format!("{}v{version}.pdf", self.0.replace('/', ""))
    }

    /// the version's pdf where arXiv sends programs, for a paper the mirror does not hold.
    fn pdf(&self, version: u32) -> Result<Remote, Error> {
        let url = Url::parse(&format!("{PDFS}{}v{version}", self.0)).map_err(|err| {
            Error::new(
                Kind::ReferenceInvalid,
                format!("arxiv:{} makes no valid link: {err}", self.0),
            )
        })?;
        Ok(Remote {
            path: self.file(version),
            size: None,
            sha256: None,
            md5: None,
            url,
            named_by_server: false,
            etag: None,
        })
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// an archive of arXiv's old ids: lowercase words joined by `-`, as `hep-th` or `cond-mat`.
fn is_archive(text: &str) -> bool {
    !text.is_empty()
        && text
            .split('-')
            .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_lowercase()))
}

/// a paper, at a version or at its newest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Paper {
    id: Id,
    version: Option<u32>,
}

impl Paper {
    /// the paper at `version`.
    fn at(&self, version: u32) -> Paper {
        Paper {
            id: self.id.clone(),
            version: Some(version),
        }
    }

    /// the paper's newest version, as a search offers it.
    fn newest(id: Id) -> Paper {
        Paper { id, version: None }
    }

    /// the paper an id without a version names, as another index records it: `1706.03762`.
    pub(super) fn named(text: &str) -> Option<Paper> {
        match Id::versioned(text)? {
            (id, None) => Some(Paper::newest(id)),
            (_, Some(_)) => None,
        }
    }

    /// its id, without a version.
    pub(super) fn id(&self) -> &Id {
        &self.id
    }
}

impl fmt::Display for Paper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "arxiv:{}", self.id)?;
        if let Some(version) = self.version {
            write!(f, "@v{version}")?;
        }
        Ok(())
    }
}

/// the papers whose titles hold `words` as a phrase, the most relevant first, and when fewer than
/// `limit` do, those that hold it anywhere: a title is how people know a paper.
pub(crate) async fn search(
    api: &impl Api,
    words: &[String],
    limit: usize,
) -> Result<Vec<Found>, Error> {
    let phrase = words.join(" ");
    if phrase.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = feed::search(api, &format!("ti:\"{phrase}\""), limit).await?;
    if found.len() < limit {
        for entry in feed::search(api, &format!("all:\"{phrase}\""), limit).await? {
            if found.len() < limit && !found.iter().any(|known| known.id == entry.id) {
                found.push(entry);
            }
        }
    }
    Ok(found
        .into_iter()
        .map(|entry| Found {
            note: entry.byline(),
            reference: Reference::Arxiv(Paper::newest(entry.id)),
            title: entry.title,
        })
        .collect())
}

#[cfg(test)]
mod tests;
