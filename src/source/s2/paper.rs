//! a paper of semantic scholar's academic graph, by any id its API takes, pinned at the
//! open-access pdf semantic scholar knows: through arXiv's own source for an arXiv paper, else at
//! the link semantic scholar gives.

use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{API, answer, busy};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Access, Found, Listing, Reference, Remote, arxiv};

/// the fields asked of a paper, and of each one a search finds.
pub(super) const FIELDS: &str = "paperId,corpusId,externalIds,url,title,year,authors,openAccessPdf";

/// the prefixes semantic scholar's API takes before an id from another index, spelled as it
/// spells them.
const PREFIXES: [&str; 7] = ["DOI", "ARXIV", "CorpusId", "MAG", "ACL", "PMID", "PMCID"];

/// a paper as semantic scholar's API takes it: its own id of 40 hexadecimal digits, as
/// `204e3073870fae3d05bcbc2f6a8e263d9b72e776`, or an id from another index after its prefix, as
/// `DOI:10.1371/journal.pcbi.1003285`, `ARXIV:1706.03762` or `CorpusId:13756489`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Paper {
    id: String,
}

impl Paper {
    /// reads a paper's id, with any prefix in any case. none for anything else, such as a
    /// dataset's name.
    pub(super) fn parse(text: &str) -> Option<Paper> {
        if is_own(text) {
            return Some(Paper {
                id: text.to_ascii_lowercase(),
            });
        }
        let (prefix, rest) = text.split_once(':')?;
        let prefix = PREFIXES
            .into_iter()
            .find(|known| known.eq_ignore_ascii_case(prefix.trim()))?;
        let rest = rest.trim();
        let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
        let id = match prefix {
            "DOI" => (rest.starts_with("10.")
                && rest.contains('/')
                && !rest.contains(char::is_whitespace))
            .then(|| rest.to_ascii_lowercase())?,
            "ARXIV" => arxiv::Paper::named(rest)?.id().to_string(),
            "PMCID" => {
                let number = rest
                    .get(..3)
                    .filter(|head| head.eq_ignore_ascii_case("pmc"))
                    .map_or(rest, |_| &rest[3..]);
                digits(number).then(|| number.to_owned())?
            }
            "ACL" => (!rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_')))
            .then(|| rest.to_owned())?,
            _ => digits(rest).then(|| rest.to_owned())?,
        };
        Some(Paper {
            id: format!("{prefix}:{id}"),
        })
    }

    /// a pasted link to a paper's page on semanticscholar.org, `/paper/<title>/<id>` or
    /// `/paper/<id>`, or to a paper in its API.
    pub(super) fn from_link(url: &Url) -> Option<Paper> {
        let parts: Vec<&str> = url
            .path_segments()?
            .filter(|part| !part.is_empty())
            .collect();
        match parts.as_slice() {
            ["paper", .., id] if is_own(id) => Paper::parse(id),
            ["graph", "v1", "paper", rest @ ..] if !rest.is_empty() => {
                Paper::parse(&rest.join("/"))
            }
            _ => None,
        }
    }

    /// the link that asks semantic scholar's API about the paper, with the fields fetchloom reads.
    fn asked(&self) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("{API}/graph/v1/paper/")).map_err(|err| {
            Error::new(
                Kind::ReferenceInvalid,
                format!("could not make a link for {self}: {err}"),
            )
        })?;
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().extend(self.id.split('/'));
        }
        url.query_pairs_mut().append_pair("fields", FIELDS);
        Ok(url)
    }

    /// lists the paper's open-access pdf at the link semantic scholar gives, with its license
    /// where it states one. a paper on arXiv without another pdf comes from arXiv, through its own
    /// source.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let url = self.asked()?;
        let response = api.get(&url).await.map_err(busy)?;
        if response.status == 404 {
            return Err(self.missing());
        }
        let paper: Answer = answer(&self.to_string(), &response)?;
        let link = paper.pdf();
        let Some(pdf) = link
            .as_ref()
            .filter(|pdf| !is_arxiv(&pdf.url) && !is_page(&pdf.url))
        else {
            let arxiv = paper
                .arxiv()
                .or_else(|| arxiv::from_link(&link.as_ref()?.url));
            return match arxiv {
                Some(arxiv) => arxiv.resolve(api).await,
                None => Err(paper.closed(self, link.as_ref().map(|pdf| &pdf.url))),
            };
        };
        let pdf_url = pdf.url.clone();
        let license = pdf.license.as_deref().map(license);
        Ok(Listing {
            within: Vec::new(),
            access: if license.as_deref().is_some_and(is_open) {
                Access::Public
            } else {
                Access::Unknown
            },
            resolved: Reference::S2Paper(Paper {
                id: paper.paper_id.to_ascii_lowercase(),
            }),
            title: paper.title.clone().filter(|title| !title.trim().is_empty()),
            license,
            doi: paper.doi().map(str::to_owned),
            files: vec![Remote {
                path: paper.file(),
                size: None,
                sha256: None,
                md5: None,
                url: pdf_url,
                named_by_server: false,
                etag: None,
            }],
            unavailable: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// the error for a paper semantic scholar does not know.
    fn missing(&self) -> Error {
        Error::new(
            Kind::ReferenceUnresolved,
            format!("semantic scholar knows no paper {self}"),
        )
        .help("check the id, or find the paper with `fetchloom search <words> --source s2`")
    }
}

impl fmt::Display for Paper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s2:{}", self.id)
    }
}

/// whether `text` is a paper id of semantic scholar's own: 40 hexadecimal digits.
fn is_own(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// whether a pdf's link is on arXiv, whose papers come through arXiv's own source: from its
/// mirror, at the pace its terms ask.
fn is_arxiv(url: &Url) -> bool {
    url.host_str()
        .is_some_and(|host| host == "arxiv.org" || host.ends_with(".arxiv.org"))
}

/// whether a link semantic scholar gives as a pdf is a DOI's, which leads to a publisher's page
/// rather than to the file.
fn is_page(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("doi.org" | "dx.doi.org" | "www.doi.org")
    )
}

/// a license as semantic scholar writes it, `CCBYNC`, in the words people and the license check
/// know, `CC-BY-NC`; any other kept as it is, as `publisher-specific-oa`.
pub(super) fn license(said: &str) -> String {
    let said = said.trim();
    let upper = said.to_ascii_uppercase();
    if upper == "CC0" {
        return "CC0-1.0".to_owned();
    }
    let Some(mut rest) = upper.strip_prefix("CCBY") else {
        return said.to_owned();
    };
    let mut named = "CC-BY".to_owned();
    while !rest.is_empty() {
        let Some(term) = ["NC", "ND", "SA"]
            .into_iter()
            .find(|term| rest.starts_with(term))
        else {
            return said.to_owned();
        };
        named.push('-');
        named.push_str(term);
        rest = &rest[term.len()..];
    }
    named
}

/// whether a license lets anyone read the paper: a creative commons one, or the public domain.
fn is_open(license: &str) -> bool {
    license.starts_with("CC") || license.eq_ignore_ascii_case("public-domain")
}

/// what semantic scholar's API says of a paper.
#[derive(Deserialize)]
pub(super) struct Answer {
    #[serde(rename = "paperId")]
    paper_id: String,
    #[serde(rename = "corpusId")]
    corpus_id: Option<u64>,
    #[serde(rename = "externalIds")]
    external: Option<External>,
    url: Option<String>,
    title: Option<String>,
    year: Option<u32>,
    authors: Option<Vec<Author>>,
    #[serde(rename = "openAccessPdf")]
    open_access_pdf: Option<OpenAccessPdf>,
}

#[derive(Deserialize)]
struct External {
    #[serde(rename = "ArXiv")]
    arxiv: Option<String>,
    #[serde(rename = "DOI")]
    doi: Option<String>,
}

#[derive(Deserialize)]
struct Author {
    name: Option<String>,
}

#[derive(Deserialize)]
struct OpenAccessPdf {
    url: Option<String>,
    license: Option<String>,
}

/// an open-access pdf semantic scholar links to.
struct Pdf {
    url: Url,
    license: Option<String>,
}

impl Answer {
    /// its open-access pdf, when semantic scholar gives a link to one. an empty link only says it
    /// knows of a copy it cannot link.
    fn pdf(&self) -> Option<Pdf> {
        let pdf = self.open_access_pdf.as_ref()?;
        let url = Url::parse(pdf.url.as_deref()?.trim()).ok()?;
        matches!(url.scheme(), "https" | "http").then(|| Pdf {
            url,
            license: pdf
                .license
                .clone()
                .filter(|license| !license.trim().is_empty()),
        })
    }

    /// the arXiv paper it is, at its newest version.
    fn arxiv(&self) -> Option<arxiv::Paper> {
        arxiv::Paper::named(self.external.as_ref()?.arxiv.as_deref()?)
    }

    fn doi(&self) -> Option<&str> {
        self.external.as_ref()?.doi.as_deref()
    }

    /// the name of its pdf: the part of its DOI after the publisher's prefix, as publishers name
    /// their pdfs, `journal.pcbi.1003285.pdf`, else its corpus id, `2180281.pdf`.
    fn file(&self) -> String {
        let stem = self
            .doi()
            .and_then(|doi| doi.split_once('/'))
            .map(|(_, rest)| {
                rest.chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                            c
                        } else {
                            '-'
                        }
                    })
                    .collect::<String>()
            })
            .filter(|stem| stem.bytes().any(|b| b.is_ascii_alphanumeric()))
            .or_else(|| self.corpus_id.map(|id| id.to_string()))
            .unwrap_or_else(|| self.paper_id.to_ascii_lowercase());
        format!("{stem}.pdf")
    }

    /// the error for a paper semantic scholar links no open-access pdf of, naming its page, or
    /// the publisher's `page` it links instead.
    fn closed(&self, paper: &Paper, page: Option<&Url>) -> Error {
        let what = match self.title.as_deref() {
            Some(title) => format!("\"{title}\" ({paper})"),
            None => paper.to_string(),
        };
        let error = Error::new(
            Kind::ReferenceUnresolved,
            format!("semantic scholar links no open-access pdf of {what}"),
        );
        if let Some(page) = page {
            return error.help(format!(
                "it links the publisher's page, {page}, which is no pdf: download the paper from there in a browser"
            ));
        }
        let page = self
            .url
            .clone()
            .unwrap_or_else(|| format!("https://www.semanticscholar.org/paper/{}", self.paper_id));
        error.help(format!(
            "its page, {page}, says where it is published, which may need a subscription"
        ))
    }

    /// the paper as a search shows it, by its corpus id, the shortest id semantic scholar gives.
    pub(super) fn found(self) -> Found {
        let id = match self.corpus_id {
            Some(corpus) => format!("CorpusId:{corpus}"),
            None => self.paper_id.to_ascii_lowercase(),
        };
        let authors: Vec<String> = self
            .authors
            .iter()
            .flatten()
            .filter_map(|author| author.name.clone())
            .collect();
        let year = self.year.map(|year| year.to_string());
        let byline = crate::source::byline(&authors, year.as_deref());
        let addable = self.arxiv().is_some() || self.pdf().is_some_and(|pdf| !is_page(&pdf.url));
        let note = if addable {
            byline
        } else {
            Some(match byline {
                Some(byline) => format!("{byline}, no open-access pdf"),
                None => "no open-access pdf".to_owned(),
            })
        };
        Found {
            reference: Reference::S2Paper(Paper { id }),
            title: self.title.unwrap_or_default(),
            note,
        }
    }
}

/// what semantic scholar's search answers.
#[derive(Deserialize)]
pub(super) struct Searched {
    pub(super) data: Option<Vec<Answer>>,
}
