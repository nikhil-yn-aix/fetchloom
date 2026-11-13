//! the papers of a month, from the mirror's folder for the month, and the papers of one category
//! first submitted in a month, through arXiv's API, each at its newest version.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use super::{FORMS, Id, feed, is_archive, mirrored};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Access, Listing, Pages, Reference, Remote, malformed};

/// the papers one page of arXiv's API lists, the most its manual advises a program to ask for.
const PAGE: usize = 1000;
/// the most papers arXiv's API lists for one query.
const MOST: u64 = 30_000;
/// what a person should know of the papers of a month: their licenses differ.
pub(super) const LICENSES: &str = "most arXiv papers come under arXiv's own license, which does not allow sharing copies; each paper's page names its license";

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// the papers whose ids arXiv gave in a month, as the mirror files them, or the papers of one
/// category whose first version was submitted in a month, as arXiv's API dates them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Month {
    /// an archive, as `hep-th`, or an archive and a subject class, as `cs.LG`.
    category: Option<String>,
    /// the month as arXiv's ids write it: `2401`.
    month: String,
}

impl Month {
    /// reads a month, `2401`, or a category and a month, `cs.LG/2401`.
    pub(super) fn parse(text: &str) -> Result<Month, Error> {
        let bad = || malformed(&format!("arxiv:{text}"), FORMS);
        let (category, month) = match text.rsplit_once('/') {
            Some((category, month)) => (Some(category_of(category).ok_or_else(bad)?), month),
            None => (None, text),
        };
        let year = Month::year(month).ok_or_else(bad)?;
        if category.is_none() && (year < 2007 || month < "0704") {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!(
                    "arxiv:{month} is before april 2007, when arXiv numbered papers within each archive"
                ),
            )
            .help(format!(
                "add one archive's month from the mirror, as gs://arxiv-dataset/arxiv/hep-th/pdf/{month}/, or one category's, as arxiv:hep-th/{month}"
            )));
        }
        Ok(Month {
            category,
            month: month.to_owned(),
        })
    }

    /// the year of a month written as arXiv's ids write it, `YYMM`, from 1991 to 2090.
    pub(super) fn year(month: &str) -> Option<u32> {
        if month.len() != 4 || !month.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let year: u32 = month[..2].parse().ok()?;
        let number: u32 = month[2..].parse().ok()?;
        (1..=12)
            .contains(&number)
            .then_some(if year >= 91 { 1900 + year } else { 2000 + year })
    }

    /// the year and the month's number, as 2024 and 1 for `2401`.
    fn date(&self) -> (u32, u32) {
        let year = Month::year(&self.month).unwrap_or_default();
        (year, self.month[2..].parse().unwrap_or(1))
    }

    /// what the listing is, as data.lock and the story name it: `arXiv cs.LG, January 2024`.
    fn title(&self) -> String {
        match &self.category {
            Some(category) => format!("arXiv {category}, {}", self.title_month()),
            None => format!("arXiv, {}", self.title_month()),
        }
    }

    /// lists each paper's pdf at its newest version.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let mut warnings = Vec::new();
        let files = match &self.category {
            None => self.mirror(api, &mut warnings).await?,
            Some(category) => self.category(api, category, &mut warnings).await?,
        };
        warnings.push(LICENSES.to_owned());
        Ok(Listing {
            within: Vec::new(),
            access: Access::Unknown,
            resolved: Reference::ArxivMonth(self.clone()),
            title: Some(self.title()),
            license: None,
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings,
        })
    }

    /// the mirror's folder for the month, each paper at the newest version it holds.
    async fn mirror(
        &self,
        api: &impl Api,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Remote>, Error> {
        let held = mirrored(api, &format!("arxiv/arxiv/pdf/{}/", self.month)).await?;
        let mut newest: BTreeMap<Id, (u32, Remote)> = BTreeMap::new();
        let mut others = 0;
        for remote in held {
            let Some((id, version)) = remote
                .path
                .strip_suffix(".pdf")
                .and_then(Id::versioned)
                .and_then(|(id, version)| Some((id, version?)))
                .filter(|(id, _)| id.month() == self.month)
            else {
                others += 1;
                continue;
            };
            if newest.get(&id).is_none_or(|(known, _)| *known < version) {
                newest.insert(id, (version, remote));
            }
        }
        if newest.is_empty() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("the mirror holds no papers of {self} yet"),
            )
            .help(format!(
                "the mirror takes in new papers about once a week, so try again later; arxiv:cs.LG/{} lists one category's papers through arXiv's own API",
                self.month
            )));
        }
        if others > 0 {
            warnings.push(format!(
                "left out {} in the mirror's folder for {self}, since {} no paper",
                crate::units::count(others, "object", "objects"),
                if others == 1 { "it names" } else { "they name" }
            ));
        }
        Ok(newest
            .into_iter()
            .map(|(id, (version, mut remote))| {
                remote.path = id.file(version);
                remote
            })
            .collect())
    }

    /// the papers of `category` first submitted in the month, page by page from arXiv's API,
    /// then each pdf from the mirror where it holds that version.
    async fn category(
        &self,
        api: &impl Api,
        category: &str,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Remote>, Error> {
        let (year, number) = self.date();
        let (next_year, next) = if number == 12 {
            (year + 1, 1)
        } else {
            (year, number + 1)
        };
        // the range takes its end minute too, so a paper of that minute is left out below
        let query = format!(
            "cat:{category} AND submittedDate:[{year}{number:02}010000 TO {next_year}{next:02}010000]"
        );
        let first = format!("{year}-{number:02}");
        let mut papers: Vec<(Id, u32)> = Vec::new();
        let mut pages = Pages::new();
        let mut start = 0_usize;
        loop {
            pages
                .next(start, papers.len())
                .map_err(|reason| feed::unusable(self, &reason))?;
            let (from, most) = (start.to_string(), PAGE.to_string());
            let pairs = [
                ("search_query", query.as_str()),
                ("sortBy", "submittedDate"),
                ("sortOrder", "ascending"),
                ("start", from.as_str()),
                ("max_results", most.as_str()),
            ];
            let page = feed::ask(api, &pairs, self).await?;
            if page.total > MOST {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "arXiv's API lists at most {} papers for one query, and {self} has {}",
                        crate::units::grouped(MOST),
                        crate::units::grouped(page.total)
                    ),
                )
                .help(format!(
                    "add the whole month from the mirror with arxiv:{}",
                    self.month
                )));
            }
            if page.entries.is_empty() && (start as u64) < page.total {
                return Err(Error::new(
                    Kind::NetworkStatus,
                    format!(
                        "arXiv's API sent no papers at {start} of the {} in {self}",
                        page.total
                    ),
                )
                .help("arXiv's API sometimes sends an empty page when it is busy: run the same command again later"));
            }
            start += page.entries.len();
            for entry in page.entries {
                let fits = entry.published.starts_with(&first);
                if fits && !papers.iter().any(|(id, _)| *id == entry.id) {
                    papers.push((entry.id, entry.version));
                }
            }
            if start as u64 >= page.total {
                break;
            }
        }
        if papers.is_empty() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("arXiv lists no papers in {category} first submitted in {}", self.title_month()),
            )
            .help("check the category as arXiv writes it, such as cs.LG or hep-th, at https://arxiv.org/category_taxonomy"));
        }
        let found = pdfs(api, papers).await?;
        if let Some(first) = found.withdrawn.first() {
            warnings.push(format!(
                "left out {} whose newest version is a withdrawal, which has no pdf, as {first}; an earlier version can be added by its id",
                crate::units::count(found.withdrawn.len() as u64, "paper", "papers")
            ));
        }
        if found.unmirrored > 0 {
            warnings.push(format!(
                "the mirror does not hold {} at its newest version yet, so {} from export.arxiv.org, one every 3 seconds",
                crate::units::count(found.unmirrored as u64, "paper", "papers"),
                if found.unmirrored == 1 { "it comes" } else { "they come" }
            ));
        }
        Ok(found.files)
    }

    /// the month alone, as `January 2024`.
    fn title_month(&self) -> String {
        let (year, number) = self.date();
        let name = MONTHS
            .get(number.saturating_sub(1) as usize)
            .copied()
            .unwrap_or_default();
        format!("{name} {year}")
    }
}

/// the pdfs of a category's papers, and what is not from the mirror.
struct Found {
    files: Vec<Remote>,
    /// the papers whose newest version is from export.arxiv.org, the mirror not holding it.
    unmirrored: usize,
    /// the papers left out, their newest version a withdrawal, each as `arxiv:<id>@v<version>`.
    withdrawn: Vec<String>,
}

/// each paper's pdf at its version: the mirror's where it holds that version, else the one at
/// export.arxiv.org. the mirror's folder for each month of their ids is listed whole; a paper
/// the mirror holds at an earlier version only may have been withdrawn since, which arXiv's
/// record of it tells, and a withdrawal has no pdf.
async fn pdfs(api: &impl Api, papers: Vec<(Id, u32)>) -> Result<Found, Error> {
    let mut folders: BTreeMap<String, Vec<(Id, u32)>> = BTreeMap::new();
    for (id, version) in papers {
        folders.entry(id.folder()).or_default().push((id, version));
    }
    let mut found = Found {
        files: Vec::new(),
        unmirrored: 0,
        withdrawn: Vec::new(),
    };
    for (folder, papers) in folders {
        let mut held: HashMap<String, Remote> = mirrored(api, &folder)
            .await?
            .into_iter()
            .map(|remote| (remote.path.clone(), remote))
            .collect();
        for (id, version) in papers {
            if let Some(mut remote) = held.remove(&id.mirrored(version)) {
                remote.path = id.file(version);
                found.files.push(remote);
                continue;
            }
            let earlier = (1..version).any(|older| held.contains_key(&id.mirrored(older)));
            if earlier && feed::record(api, &id).await?.withdraws(version) {
                found.withdrawn.push(format!("arxiv:{id}@v{version}"));
                continue;
            }
            found.unmirrored += 1;
            found.files.push(id.pdf(version)?);
        }
    }
    found.files.sort_by(|one, other| one.path.cmp(&other.path));
    found.withdrawn.sort();
    Ok(found)
}

/// an arXiv category as arXiv writes it: an archive alone, as `hep-th`, or with a subject class,
/// two capitals as in `cs.LG` or lowercase words as in `cond-mat.mes-hall`.
fn category_of(text: &str) -> Option<String> {
    let (archive, subject) = match text.split_once('.') {
        Some((archive, subject)) => (archive, Some(subject)),
        None => (text, None),
    };
    let archive = archive.to_ascii_lowercase();
    if !is_archive(&archive) {
        return None;
    }
    let Some(subject) = subject else {
        return Some(archive);
    };
    let words = !subject.is_empty()
        && subject
            .split('-')
            .all(|word| !word.is_empty() && word.bytes().all(|b| b.is_ascii_alphabetic()));
    if !words {
        return None;
    }
    let subject = if subject.len() == 2 {
        subject.to_ascii_uppercase()
    } else {
        subject.to_ascii_lowercase()
    };
    Some(format!("{archive}.{subject}"))
}

impl fmt::Display for Month {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.category {
            Some(category) => write!(f, "arxiv:{category}/{}", self.month),
            None => write!(f, "arxiv:{}", self.month),
        }
    }
}
